//! Native archive execution. One worker owns one open session; the parent owns output.
mod output;
use crate::{
    backend::ArchiveAdapter,
    member::*,
    types::*,
    worker::{self, Event, Frame, Operation},
};
use async_trait::async_trait;
use output::{ControlledOutput, Target};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use smartzip_core::{
    AdapterCapabilities, ArchiveFormat, EncodingMode, Result, SmartZipError, TaskExecutionContext,
};
use smartzip_sevenzip::{Entry, RunReport, RuntimeInfo};
use std::{
    borrow::Cow,
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone, Default)]
pub struct SevenZipLocator {
    bundled: Option<PathBuf>,
}
impl SevenZipLocator {
    pub fn bundled(path: PathBuf) -> Self {
        Self {
            bundled: Some(path),
        }
    }
    pub fn locate(&self) -> Option<PathBuf> {
        self.locate_all().into_iter().next()
    }
    pub fn locate_all(&self) -> Vec<PathBuf> {
        if let Some(path) = &self.bundled {
            return vec![absolute(path)];
        }
        if let Some(path) = std::env::var_os("SMARTZIP_7Z_LIBRARY") {
            return vec![absolute(Path::new(&path))];
        }
        let mut paths = Vec::new();
        let filename = if cfg!(windows) {
            "7z.dll"
        } else if cfg!(target_os = "macos") {
            "7z.dylib"
        } else {
            "7z.so"
        };
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for base in [
                    dir.to_path_buf(),
                    dir.join("lib"),
                    dir.join("../lib/smartzip"),
                ] {
                    paths.push(base.join(filename));
                }
            }
        }
        paths
            .into_iter()
            .filter(|p| p.is_file())
            .map(|p| absolute(&p))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
fn absolute(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.into()))
}
#[derive(Debug, Clone)]
pub struct SevenZipBackend {
    id: String,
    library: PathBuf,
    info: std::result::Result<RuntimeInfo, String>,
}
impl SevenZipBackend {
    pub fn new(path: PathBuf) -> Self {
        let library = absolute(&path);
        let info = smartzip_sevenzip::Library::load(&library)
            .map(|l| l.info().clone())
            .map_err(|e| e.to_string());
        Self {
            id: format!("sevenzip:{}", library.display()),
            library,
            info,
        }
    }
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
    pub fn locate(locator: &SevenZipLocator) -> Result<Self> {
        locator
            .locate()
            .map(Self::new)
            .ok_or_else(|| SmartZipError::BackendUnavailable {
                backend:
                    "7-Zip native library; set SMARTZIP_7Z_LIBRARY or install the bundled library"
                        .into(),
            })
    }
    pub fn library(&self) -> &Path {
        &self.library
    }
    pub fn runtime_info(&self) -> std::result::Result<&RuntimeInfo, &str> {
        self.info.as_ref().map_err(String::as_str)
    }
    fn protocol(&self, detail: impl Into<String>) -> SmartZipError {
        SmartZipError::BackendProtocolError {
            backend: self.id.clone(),
            detail: detail.into(),
        }
    }
    async fn execute(
        &self,
        path: &Path,
        format: Option<&ArchiveFormat>,
        password: Option<&str>,
        encoding: &EncodingMode,
        operation: Operation,
        context: Arc<TaskExecutionContext>,
        target: Target,
    ) -> Result<Executed> {
        if context.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        if let Err(error) = &self.info {
            return Err(SmartZipError::BackendUnavailable {
                backend: format!("{}: {error}", self.id),
            });
        }
        let input = crate::source::resolve_input(path, context.archive_input())
            .map_err(|e| SmartZipError::io(Some(path.into()), e))?;
        let catalog_id = crate::source::catalog_id(&input, &self.library)
            .map_err(|e| SmartZipError::io(Some(path.into()), e))?;
        target.check_catalog(&catalog_id)?;
        let worker = worker_path().ok_or_else(|| SmartZipError::BackendUnavailable {
            backend: "smartzip-archive-worker (build/install smartzip-archive)".into(),
        })?;
        let verify = matches!(&operation, Operation::Verify);
        let list = matches!(&operation, Operation::List);
        let decode = matches!(&operation, Operation::Decode { .. });
        let request = worker::Request {
            library: self.library.as_path().into(),
            input: (&input).into(),
            name: input
                .volumes
                .iter()
                .find(|(_, p)| *p == &input.path)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| {
                    input
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                }),
            format: format.map(|f| f.as_str().to_owned()),
            password: password.map(str::to_owned),
            operation,
        };
        let mut command = CommandWrap::with_new(&worker, |cmd| {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
        });
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
        command.wrap(KillOnDrop);
        let mut child = command
            .spawn()
            .map_err(|e| SmartZipError::io(Some(worker.clone()), e))?;
        let mut stdin = child
            .stdin()
            .take()
            .ok_or_else(|| self.protocol("worker stdin missing"))?;
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| self.protocol("worker stdout missing"))?;
        let mut stdout = BufReader::with_capacity(worker::DATA_CHUNK, stdout);
        let encoded = serde_json::to_vec(&request).map_err(|e| self.protocol(e.to_string()))?;
        let token = context.cancellation_token();
        let mut budget = context.output_budget();
        // This also tells the engine to stop polling this native output tree.
        if matches!(target, Target::Extract { .. }) {
            if let Some(b) = &budget {
                if !b.requires_admission() {
                    budget = None;
                }
            }
        }
        let operation = async {
            stdin
                .write_all(&(encoded.len() as u64).to_le_bytes())
                .await
                .map_err(|e| SmartZipError::io(Some(worker.clone()), e))?;
            stdin
                .write_all(&encoded)
                .await
                .map_err(|e| SmartZipError::io(Some(worker.clone()), e))?;
            stdin
                .shutdown()
                .await
                .map_err(|e| SmartZipError::io(Some(worker.clone()), e))?;
            let consume = async {
                let mut result = Executed::default();
                let mut native_entries = Vec::new();
                let mut data = Vec::new();
                let mut output = ControlledOutput::new(target, budget);
                loop {
                    if context.is_cancelled() {
                        return Err(SmartZipError::Cancelled);
                    }
                    let frame = read_frame(&mut stdout, &mut data)
                        .await
                        .map_err(|e| self.protocol(format!("worker stream: {e}")))?;
                    match frame {
                        None => break,
                        Some(Frame::Event(Event::Probe { format, encrypted })) => {
                            result.format = Some(parse_format(&format));
                            result.encrypted = encrypted;
                        }
                        Some(Frame::Event(Event::Catalog {
                            format,
                            item_count,
                            entries,
                        })) => {
                            let format = parse_format(&format);
                            result.encrypted = entries
                                .iter()
                                .filter_map(|e| e.encrypted)
                                .reduce(|a, b| a || b);
                            let zip = if list || output.is_extract() {
                                zip_names(&input, &format, encoding, item_count)?
                            } else {
                                None
                            };
                            if list {
                                result.entries = entries
                                    .into_iter()
                                    .map(|e| {
                                        listing_entry(e, &catalog_id, zip.as_deref(), encoding)
                                    })
                                    .collect::<Result<_>>()?;
                            } else {
                                output.prepare(&entries, |entry| {
                                    entry_name(entry, zip.as_deref(), encoding).map(Cow::into_owned)
                                })?;
                                native_entries = entries;
                            }
                            result.format = Some(format);
                        }
                        Some(Frame::Event(Event::Progress { completed, total })) => {
                            let progress = if let Some(total) = total.filter(|n| *n != 0) {
                                smartzip_core::TaskProgress::percent(
                                    (completed as f64 / total as f64 * 100.0).min(100.0) as f32,
                                    "decoding",
                                )
                            } else {
                                smartzip_core::TaskProgress::indeterminate("decoding")
                            };
                            context.emit_progress(progress);
                        }
                        Some(Frame::End(index)) => {
                            output.close(index)?;
                        }
                        Some(Frame::Data { index, bytes }) => {
                            if output.write(index, bytes)? {
                                result.prefix_satisfied = true;
                                break;
                            }
                        }
                        Some(Frame::Event(Event::Report {
                            report,
                            verification,
                        })) => {
                            result.report = Some(report);
                            result.verification = verification;
                        }
                        Some(Frame::Event(Event::Error(error))) => {
                            if verify {
                                result.open_error = Some(error);
                                break;
                            }
                            return Err(map_native_error(&self.id, path, password, error));
                        }
                    }
                }
                if crate::source::catalog_id(&input, &self.library)
                    .map_err(|e| SmartZipError::io(Some(path.into()), e))?
                    != catalog_id
                {
                    return Err(self.protocol("archive source changed during operation"));
                }
                result.bytes = std::mem::take(&mut output.memory);
                if result.prefix_satisfied {
                    return Ok(result);
                }
                if decode {
                    let report = result
                        .report
                        .as_ref()
                        .ok_or_else(|| self.protocol("worker did not return operation report"))?;
                    let complete = worker::successful_indices(report).is_some_and(|passed| {
                        native_entries
                            .iter()
                            .all(|e| e.is_dir || passed.contains(&e.index))
                    });
                    check_report(
                        &self.id,
                        path,
                        password,
                        report,
                        complete,
                        result.encrypted,
                        &native_entries,
                    )?;
                    result.manifest = output.finish()?;
                }
                Ok(result)
            };
            let result = consume.await?;
            Ok::<_, SmartZipError>(result)
        };
        let result = tokio::select! {biased;_=token.cancelled()=>Err(SmartZipError::Cancelled),value=operation=>value};
        // Prefix satisfaction and every failure end the native lifetime before returning.
        let must_kill = result.as_ref().map_or(true, |r| r.prefix_satisfied);
        if must_kill {
            let _ = child.start_kill();
        }
        let status = child
            .wait()
            .await
            .map_err(|e| SmartZipError::io(Some(worker), e))?;
        let result = result?;
        if !must_kill && !status.success() {
            return Err(self.protocol(format!("native worker exited: {status}")));
        }
        if result.open_error.is_none() {
            if verify && (result.report.is_none() || result.verification.is_none()) {
                return Err(self.protocol("worker did not return verification"));
            }
            if !verify && result.format.is_none() {
                return Err(self.protocol("worker did not return a catalog"));
            }
        }
        Ok(result)
    }
}
fn worker_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SMARTZIP_ARCHIVE_WORKER") {
        return Some(p.into());
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "smartzip-archive-worker.exe"
    } else {
        "smartzip-archive-worker"
    };
    [dir.join(name), dir.join("..").join(name)]
        .into_iter()
        .find(|p| p.is_file())
}
async fn read_frame<'a>(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    data: &'a mut Vec<u8>,
) -> io::Result<Option<Frame<'a>>> {
    let mut kind = [0];
    if stream.read(&mut kind).await? == 0 {
        return Ok(None);
    }
    match kind[0] {
        0 => {
            let len = usize::try_from(stream.read_u64_le().await?).map_err(io::Error::other)?;
            let mut bytes = vec![0; len];
            stream.read_exact(&mut bytes).await?;
            Ok(Some(Frame::Event(
                serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            )))
        }
        1 => {
            let index = stream.read_u32_le().await?;
            let len = stream.read_u32_le().await? as usize;
            if len > worker::DATA_CHUNK {
                return Err(io::Error::other("invalid data frame length"));
            }
            data.resize(len, 0);
            stream.read_exact(data).await?;
            Ok(Some(Frame::Data { index, bytes: data }))
        }
        2 => Ok(Some(Frame::End(stream.read_u32_le().await?))),
        _ => Err(io::Error::other("unknown native frame")),
    }
}
#[derive(Default)]
struct Executed {
    open_error: Option<smartzip_sevenzip::Error>,
    format: Option<ArchiveFormat>,
    entries: Vec<ArchiveEntry>,
    encrypted: Option<bool>,
    report: Option<RunReport>,
    verification: Option<worker::Verification>,
    manifest: Option<OutputManifest>,
    bytes: Vec<u8>,
    prefix_satisfied: bool,
}
fn zip_names(
    input: &smartzip_core::ArchiveInput,
    format: &ArchiveFormat,
    encoding: &EncodingMode,
    item_count: u32,
) -> Result<Option<Vec<crate::zip_names::ZipName>>> {
    if *format != ArchiveFormat::Zip || !matches!(encoding, EncodingMode::Override(_)) {
        return Ok(None);
    }
    let entries = crate::zip_names::read(input)?;
    if entries.len() != item_count as usize {
        return Err(SmartZipError::BackendProtocolError {
            backend: "native-name-view".into(),
            detail: "ZIP metadata and native index count differ".into(),
        });
    }
    Ok(Some(entries))
}

fn entry_name<'a>(
    entry: &'a Entry,
    zip: Option<&'a [crate::zip_names::ZipName]>,
    encoding: &EncodingMode,
) -> Result<Cow<'a, str>> {
    let (Some(zip), EncodingMode::Override(enc)) = (zip, encoding) else {
        return Ok(Cow::Borrowed(&entry.name));
    };
    zip[entry.index as usize]
        .decode(enc)
        .ok_or_else(|| SmartZipError::BackendProtocolError {
            backend: "native-name-view".into(),
            detail: format!("ZIP name cannot be decoded as {enc}"),
        })
}

fn listing_entry(
    mut entry: Entry,
    id: &str,
    zip: Option<&[crate::zip_names::ZipName]>,
    encoding: &EncodingMode,
) -> Result<ArchiveEntry> {
    let path = if zip.is_none() {
        std::mem::take(&mut entry.name).into()
    } else {
        entry_name(&entry, zip, encoding)?.into_owned().into()
    };
    Ok(ArchiveEntry {
        native: Some(NativeEntry {
            reference: EntryRef {
                catalog_id: id.into(),
                index: entry.index,
            },
            kind: if entry.is_dir {
                EntryKind::Directory
            } else if entry.hard_link.is_some() {
                EntryKind::HardLink
            } else if entry.link.is_some()
                || entry.unix_mode.is_some_and(|m| m & 0o170000 == 0o120000)
            {
                EntryKind::SymbolicLink
            } else {
                EntryKind::File
            },
            encrypted: entry.encrypted,
            method: entry.method,
            crc: entry.crc,
            block: entry.block,
        }),
        path,
        compressed_size: entry.packed_size,
        uncompressed_size: entry.size,
        is_dir: entry.is_dir,
    })
}
fn parse_format(name: &str) -> ArchiveFormat {
    match name.to_ascii_lowercase().as_str() {
        "7z" => ArchiveFormat::SevenZip,
        "zip" => ArchiveFormat::Zip,
        "rar" | "rar5" => ArchiveFormat::Rar,
        "tar" => ArchiveFormat::Tar,
        "gzip" => ArchiveFormat::Gzip,
        "bzip2" => ArchiveFormat::Bzip2,
        "xz" => ArchiveFormat::Xz,
        "zstd" => ArchiveFormat::Zstd,
        "cab" => ArchiveFormat::Cab,
        "iso" => ArchiveFormat::Iso,
        "dmg" => ArchiveFormat::Dmg,
        "lz4" => ArchiveFormat::Lz4,
        "lzma" => ArchiveFormat::Lzma,
        _ => ArchiveFormat::Unknown(name.to_ascii_lowercase()),
    }
}
fn stream_error(error: &smartzip_sevenzip::StreamError, path: &Path) -> SmartZipError {
    let kind = match error.kind.as_str() {
        "NotFound" => io::ErrorKind::NotFound,
        "PermissionDenied" => io::ErrorKind::PermissionDenied,
        "UnexpectedEof" => io::ErrorKind::UnexpectedEof,
        "Interrupted" => io::ErrorKind::Interrupted,
        _ => io::ErrorKind::Other,
    };
    SmartZipError::io(
        Some(path.into()),
        io::Error::new(
            kind,
            format!(
                "{}{}: {}",
                error.operation,
                error
                    .name
                    .as_ref()
                    .map(|n| format!(" {n}"))
                    .unwrap_or_default(),
                error.message
            ),
        ),
    )
}
fn map_native_error(
    id: &str,
    path: &Path,
    password: Option<&str>,
    error: smartzip_sevenzip::Error,
) -> SmartZipError {
    if let Some(e) = error.stream_errors.first() {
        return stream_error(e, path);
    }
    if error.password_requested {
        return if password.is_none() {
            SmartZipError::PasswordRequired { path: path.into() }
        } else {
            SmartZipError::PasswordIndeterminate { path: path.into() }
        };
    }
    if error.flags.errors.unwrap_or(0) != 0 {
        return SmartZipError::CorruptedArchive {
            path: path.into(),
            detail: error.message,
        };
    }
    if error.hresult == Some(1) {
        SmartZipError::UnsupportedContainer {
            backend: id.into(),
            path: path.into(),
            container: None,
        }
    } else {
        SmartZipError::BackendProtocolError {
            backend: id.into(),
            detail: error.message,
        }
    }
}

fn check_report(
    id: &str,
    path: &Path,
    password: Option<&str>,
    report: &RunReport,
    complete: bool,
    encrypted: Option<bool>,
    entries: &[Entry],
) -> Result<()> {
    if complete {
        return Ok(());
    }
    if let Some(e) = report.stream_errors.first() {
        return Err(stream_error(e, path));
    }
    if report.items.iter().any(|i| i.result == 1) {
        return Err(SmartZipError::UnsupportedCodec {
            backend: id.into(),
            path: path.into(),
            codec: None,
        });
    }
    if report.items.iter().any(|i| i.result == 9) {
        return Err(SmartZipError::WrongPassword { path: path.into() });
    }
    let plaintext_failure = report.items.iter().any(|r| {
        r.result != 0
            && entries
                .binary_search_by_key(&r.index, |e| e.index)
                .is_ok_and(|i| entries[i].encrypted == Some(false))
    });
    let encrypted = !plaintext_failure && (encrypted == Some(true) || report.password_requested);
    if encrypted {
        return Err(if password.is_none() {
            SmartZipError::PasswordRequired { path: path.into() }
        } else {
            SmartZipError::PasswordIndeterminate { path: path.into() }
        });
    }
    Err(SmartZipError::CorruptedArchive {
        path: path.into(),
        detail: format!(
            "native HRESULT {:#x}; item results {:?}; flags {:?}",
            report.hresult, report.items, report.flags
        ),
    })
}
#[async_trait]
impl ArchiveAdapter for SevenZipBackend {
    fn id(&self) -> &str {
        &self.id
    }
    fn diagnostic_family(&self) -> Option<&'static str> {
        Some("7z")
    }
    fn runtime_diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"library":self.library,"runtime":self.info.as_ref().ok(),"error":self.info.as_ref().err()})
    }
    async fn read_member_with_context(
        &self,
        request: MemberReadRequest,
        token: tokio_util::sync::CancellationToken,
    ) -> Result<MemberReadResult> {
        let mut context = TaskExecutionContext::detached().with_cancellation(token);
        if let Some(input) = request.input {
            context = context.with_archive_input(input);
        }
        let executed = self
            .execute(
                &request.archive,
                None,
                request.password.as_deref(),
                &EncodingMode::Auto,
                Operation::Decode {
                    indices: Some(vec![request.entry.index]),
                },
                Arc::new(context),
                Target::Read {
                    entry: request.entry,
                    extent: request.extent,
                },
            )
            .await?;
        Ok(MemberReadResult {
            bytes: executed.bytes,
            complete: !executed.prefix_satisfied,
        })
    }
    async fn probe_with_context(
        &self,
        path: &Path,
        context: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveProbe> {
        match self
            .execute(
                path,
                None,
                None,
                &EncodingMode::Auto,
                Operation::Probe,
                context,
                Target::None,
            )
            .await
        {
            Ok(value) => Ok(ArchiveProbe {
                path: path.into(),
                format: value.format,
                encrypted: value.encrypted,
                supported: true,
            }),
            Err(
                SmartZipError::PasswordRequired { .. }
                | SmartZipError::PasswordIndeterminate { .. },
            ) => Ok(ArchiveProbe {
                path: path.into(),
                format: None,
                encrypted: Some(true),
                supported: true,
            }),
            Err(SmartZipError::UnsupportedContainer { .. }) => Ok(ArchiveProbe {
                path: path.into(),
                format: None,
                encrypted: None,
                supported: false,
            }),
            Err(e) => Err(e),
        }
    }
    async fn list_with_context(
        &self,
        r: ListRequest,
        c: Arc<TaskExecutionContext>,
    ) -> Result<ArchiveListing> {
        let v = self
            .execute(
                &r.archive,
                r.format.as_ref(),
                r.password.as_deref(),
                &r.encoding,
                Operation::List,
                c,
                Target::None,
            )
            .await?;
        Ok(ArchiveListing {
            format: v.format,
            entries: v.entries,
        })
    }
    async fn test_with_context(
        &self,
        r: TestRequest,
        c: Arc<TaskExecutionContext>,
    ) -> Result<TestResult> {
        use crate::integrity::{
            BackendDiagnosticDetail, BackendTestDiagnostics, Coverage, TestFailure,
        };
        let v = self
            .execute(
                &r.archive,
                r.format.as_ref(),
                r.password.as_deref(),
                &r.encoding,
                Operation::Verify,
                c.clone(),
                Target::None,
            )
            .await?;
        if let Some(error) = v.open_error {
            let failure = match map_native_error(
                &self.id,
                &r.archive,
                r.password.as_deref(),
                error.clone(),
            ) {
                SmartZipError::PasswordRequired { .. } => TestFailure::PasswordRequired,
                SmartZipError::PasswordIndeterminate { .. } => TestFailure::PasswordIndeterminate,
                SmartZipError::CorruptedArchive { .. } => TestFailure::Corruption,
                SmartZipError::Io { .. } => {
                    if error
                        .stream_errors
                        .iter()
                        .any(|e| e.operation == "volume" && e.kind == "NotFound")
                    {
                        TestFailure::MissingVolume
                    } else {
                        TestFailure::Io
                    }
                }
                _ => TestFailure::Unknown,
            };
            return Ok(TestResult {
                ok: false,
                encrypted: error.password_requested.then_some(true),
                diagnostics: BackendTestDiagnostics {
                    adapter_id: self.id.clone(),
                    family: "7z".into(),
                    version: self.info.as_ref().ok().map(|i| i.version.clone()),
                    failure: Some(failure),
                    coverage: Coverage::None,
                    damaged_files: vec![],
                    detail: BackendDiagnosticDetail::Native {
                        report: RunReport {
                            hresult: error.hresult.unwrap_or(0x80004005u32 as i32),
                            flags: error.flags,
                            password_requested: error.password_requested,
                            stream_errors: error.stream_errors,
                            ..Default::default()
                        },
                    },
                },
            });
        }
        let report = v.report.unwrap();
        let verification = v.verification.unwrap();
        let checked = check_report(
            &self.id,
            &r.archive,
            r.password.as_deref(),
            &report,
            verification.complete,
            verification.encrypted,
            &verification.failures,
        );
        let failure = match checked {
            Ok(()) => None,
            Err(SmartZipError::PasswordRequired { .. }) => Some(TestFailure::PasswordRequired),
            Err(SmartZipError::WrongPassword { .. }) => Some(TestFailure::PasswordRejected),
            Err(SmartZipError::PasswordIndeterminate { .. }) => {
                Some(TestFailure::PasswordIndeterminate)
            }
            Err(SmartZipError::UnsupportedCodec { .. }) => {
                return Err(SmartZipError::UnsupportedCodec {
                    backend: self.id.clone(),
                    path: r.archive,
                    codec: None,
                })
            }
            Err(SmartZipError::Io { .. }) => Some(
                if report
                    .stream_errors
                    .iter()
                    .any(|e| e.operation == "volume" && e.kind == "NotFound")
                {
                    TestFailure::MissingVolume
                } else {
                    TestFailure::Io
                },
            ),
            Err(_) => Some(TestFailure::Corruption),
        };
        let zip = if !verification.failures.is_empty()
            && matches!(r.encoding, EncodingMode::Override(_))
        {
            let input = crate::source::resolve_input(&r.archive, c.archive_input())
                .map_err(|e| SmartZipError::io(Some(r.archive.clone()), e))?;
            zip_names(
                &input,
                &parse_format(&verification.format),
                &r.encoding,
                verification.item_count,
            )
            .ok()
            .flatten()
        } else {
            None
        };
        let damaged_files = verification
            .failures
            .iter()
            .map(|entry| {
                entry_name(entry, zip.as_deref(), &r.encoding)
                    .unwrap_or(Cow::Borrowed(&entry.name))
                    .into_owned()
            })
            .collect();
        Ok(TestResult {
            ok: failure.is_none(),
            encrypted: verification.encrypted,
            diagnostics: BackendTestDiagnostics {
                adapter_id: self.id.clone(),
                family: "7z".into(),
                version: self.info.as_ref().ok().map(|i| i.version.clone()),
                failure,
                coverage: if failure.is_none() {
                    Coverage::Complete
                } else {
                    Coverage::Partial
                },
                damaged_files,
                detail: BackendDiagnosticDetail::Native { report },
            },
        })
    }
    async fn extract_with_context(
        &self,
        r: ExtractArchiveRequest,
        c: Arc<TaskExecutionContext>,
    ) -> Result<ExtractArchiveResult> {
        let indices = r.selection.as_ref().map(|refs| {
            let mut v: Vec<_> = refs.iter().map(|r| r.index).collect();
            v.sort_unstable();
            v.dedup();
            v
        });
        let v = self
            .execute(
                &r.archive,
                r.format.as_ref(),
                r.password.as_deref(),
                &r.encoding,
                Operation::Decode { indices },
                c,
                Target::Extract {
                    root: r.output_dir.clone(),
                    selection: r.selection,
                },
            )
            .await?;
        Ok(ExtractArchiveResult {
            output_dir: r.output_dir,
            encrypted: v.encrypted,
            manifest: v.manifest,
        })
    }
    fn capabilities(&self) -> AdapterCapabilities {
        let mut caps = crate::router::seven_zip_capabilities();
        caps.read_containers = self
            .info
            .as_ref()
            .map(|info| {
                info.formats
                    .iter()
                    .map(|f| parse_format(&f.name))
                    .filter(|f| {
                        *f != ArchiveFormat::Rar
                            || info
                                .codecs
                                .iter()
                                .any(|c| c.decoder && c.name.starts_with("Rar"))
                    })
                    .fold(Vec::new(), |mut v, f| {
                        if !v.contains(&f) {
                            v.push(f);
                        }
                        v
                    })
            })
            .unwrap_or_default();
        caps
    }
}
