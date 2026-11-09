//! Synchronous, index-based access to the official 7-Zip 26.06 library.
//!
//! Load only a trusted application-distributed native library. Sessions stay on their
//! creating thread; cancellation/progress and output factories can be shared.
mod callbacks;
mod ffi;
use callbacks::{Callback, Shared};
use ffi::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    path::{Path, PathBuf},
    ptr,
    rc::Rc,
    sync::{Arc, Mutex},
};

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
pub trait Control: Send + Sync {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _completed: u64, _total: Option<u64>) {}
}
pub trait OutputFactory: Send + Sync {
    fn open(&self, index: u32) -> io::Result<Option<Box<dyn Write + Send>>>;
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OpenOptions {
    pub format: Option<String>,
    pub name: String,
    pub password: Option<String>,
    pub volumes: BTreeMap<String, PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub version: String,
    pub formats: Vec<Format>,
    pub codecs: Vec<Codec>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Format {
    pub name: String,
    pub extensions: Vec<String>,
    pub flags: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Codec {
    pub id: Option<u64>,
    pub name: String,
    pub decoder: bool,
    pub encoder: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub index: u32,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub packed_size: Option<u64>,
    pub encrypted: Option<bool>,
    pub link: Option<String>,
    pub hard_link: Option<String>,
    pub unix_mode: Option<u32>,
    /// Windows FILETIME ticks (100 ns since 1601), when supplied by the handler.
    pub modified: Option<u64>,
    pub method: Option<String>,
    pub crc: Option<u32>,
    pub block: Option<u64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFlags {
    pub errors: Option<u32>,
    pub warnings: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemResult {
    pub index: u32,
    pub result: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraResult {
    pub index_type: u32,
    pub index: u32,
    pub result: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamError {
    pub operation: String,
    pub index: Option<u32>,
    pub name: Option<String>,
    pub kind: String,
    pub message: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    pub hresult: i32,
    pub items: Vec<ItemResult>,
    pub extra_results: Vec<ExtraResult>,
    pub flags: ArchiveFlags,
    pub password_requested: bool,
    pub stream_errors: Vec<StreamError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Error {
    pub message: String,
    pub hresult: Option<i32>,
    pub password_requested: bool,
    pub flags: ArchiveFlags,
    pub stream_errors: Vec<StreamError>,
}
impl Error {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hresult: None,
            password_requested: false,
            flags: ArchiveFlags::default(),
            stream_errors: Vec::new(),
        }
    }
    fn hr(message: impl Into<String>, hr: i32) -> Self {
        Self {
            hresult: Some(hr),
            ..Self::message(message)
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hr) = self.hresult {
            write!(f, " (HRESULT 0x{:08x})", hr as u32)?;
        }
        Ok(())
    }
}
impl std::error::Error for Error {}
struct Handler {
    class: Guid,
    format: Format,
    signature: Vec<u8>,
    multi_signature: Vec<u8>,
    signature_offset: u64,
}
struct Api {
    _library: libloading::Library,
    create: Create,
    clear: Clear,
    length: Length,
    alloc: Alloc,
}
struct PropertyValue<'a> {
    value: Variant,
    api: &'a Api,
}
impl Drop for PropertyValue<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.api.clear)(&mut self.value);
        }
    }
}
impl PropertyValue<'_> {
    fn number(&self) -> Option<u64> {
        unsafe {
            match self.value.kind {
                17 => Some(self.value.value.number & 0xff),
                18 => Some(self.value.value.number & 0xffff),
                19 => Some(self.value.value.number & 0xffffffff),
                21 => Some(self.value.value.number),
                _ => None,
            }
        }
    }
    fn filetime(&self) -> Option<u64> {
        (self.value.kind == 64).then(|| unsafe { self.value.value.number })
    }
    fn boolean(&self) -> Option<bool> {
        (self.value.kind == 11).then(|| unsafe { self.value.value.number & 0xffff != 0 })
    }
    fn bytes(&self) -> Option<Vec<u8>> {
        if self.value.kind != 8 {
            return None;
        }
        unsafe {
            let p = self.value.value.text;
            let n = (self.api.length)(p) as usize;
            if n == 0 {
                return Some(Vec::new());
            }
            Some(std::slice::from_raw_parts(p.cast(), n).to_vec())
        }
    }
    fn text(&self) -> Option<String> {
        if self.value.kind != 8 {
            return None;
        }
        unsafe {
            let p = self.value.value.text;
            let n = (self.api.length)(p) as usize / std::mem::size_of::<Wide>();
            if n == 0 {
                return Some(String::new());
            }
            Some(decode(std::slice::from_raw_parts(p, n)))
        }
    }
}
impl Api {
    fn property(&self, call: impl FnOnce(*mut Variant) -> Hr) -> Result<PropertyValue<'_>, Error> {
        let mut value = PropertyValue {
            value: Variant::empty(),
            api: self,
        };
        let hr = call(&mut value.value);
        if hr != OK {
            return Err(Error::hr("7-Zip property query failed", hr));
        }
        Ok(value)
    }
    fn password(&self, text: &str) -> Result<*mut Wide, Hr> {
        // 7z/RAR crypto consumes UTF-16 code units even when Unix wchar_t is
        // 32-bit. Keep surrogate pairs in separate slots for non-BMP passwords.
        let chars: Vec<Wide> = text.encode_utf16().map(|c| c as Wide).collect();
        let len = u32::try_from(chars.len()).map_err(|_| FAIL)?;
        let p = unsafe { (self.alloc)(chars.as_ptr(), len) };
        if p.is_null() {
            Err(0x8007000eu32 as i32)
        } else {
            Ok(p)
        }
    }
    fn string(&self, text: &str) -> Result<*mut Wide, Hr> {
        let chars = wide(text);
        let len = u32::try_from(chars.len()).map_err(|_| FAIL)?;
        let p = unsafe { (self.alloc)(chars.as_ptr(), len) };
        if p.is_null() {
            Err(0x8007000eu32 as i32)
        } else {
            Ok(p)
        }
    }
}
#[derive(Clone)]
pub struct Library {
    api: Arc<Api>,
    info: RuntimeInfo,
    handlers: Arc<Vec<Handler>>,
}
impl Library {
    pub fn load(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::message("7-Zip library path must be absolute"));
        }
        // The library is trusted executable code; ABI and pinned version are checked before COM use.
        unsafe {
            let library =
                libloading::Library::new(path).map_err(|e| Error::message(e.to_string()))?;
            macro_rules! symbol {
                ($name:literal,$ty:ty) => {
                    *library
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|e| Error::message(e.to_string()))?
                };
            }
            let create = symbol!("CreateObject", Create);
            let module = symbol!("GetModuleProp", ModuleProperty);
            let formats = symbol!("GetNumberOfFormats", Count);
            let format_prop = symbol!("GetHandlerProperty2", Property);
            let codecs = symbol!("GetNumberOfMethods", Count);
            let codec_prop = symbol!("GetMethodProperty", Property);
            #[cfg(not(windows))]
            let (clear, length, alloc) = (
                symbol!("VariantClear", Clear),
                symbol!("SysStringByteLen", Length),
                symbol!("SysAllocStringLen", Alloc),
            );
            #[cfg(windows)]
            let (clear, length, alloc) = (
                VariantClear as Clear,
                SysStringByteLen as Length,
                SysAllocStringLen as Alloc,
            );
            let api = Arc::new(Api {
                _library: library,
                create,
                clear,
                length,
                alloc,
            });
            if api.property(|v| module(0, v))?.number() != Some(0) {
                return Err(Error::message("Unsupported 7-Zip IUnknown ABI"));
            }
            if api.property(|v| module(1, v))?.number() != Some((26 << 16) | 6) {
                return Err(Error::message("Expected official 7-Zip 26.06"));
            }
            let mut count = 0;
            let hr = formats(&mut count);
            if hr != OK {
                return Err(Error::hr("Cannot enumerate archive handlers", hr));
            }
            let mut handlers = Vec::new();
            for i in 0..count {
                let name = api
                    .property(|v| format_prop(i, 0, v))?
                    .text()
                    .ok_or_else(|| Error::message("Handler has no name"))?;
                let bytes = api
                    .property(|v| format_prop(i, 1, v))?
                    .bytes()
                    .ok_or_else(|| Error::message("Handler has no class ID"))?;
                if bytes.len() != 16 {
                    return Err(Error::message("Invalid handler class ID"));
                }
                let class = ptr::read_unaligned(bytes.as_ptr().cast());
                let extensions = api
                    .property(|v| format_prop(i, 2, v))?
                    .text()
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                let flags = api
                    .property(|v| format_prop(i, 11, v))?
                    .number()
                    .unwrap_or(0) as u32;
                let signature = api
                    .property(|v| format_prop(i, 6, v))?
                    .bytes()
                    .unwrap_or_default();
                let multi_signature = api
                    .property(|v| format_prop(i, 7, v))?
                    .bytes()
                    .unwrap_or_default();
                let signature_offset = api
                    .property(|v| format_prop(i, 8, v))?
                    .number()
                    .unwrap_or(0);
                handlers.push(Handler {
                    class,
                    format: Format {
                        name,
                        extensions,
                        flags,
                    },
                    signature,
                    multi_signature,
                    signature_offset,
                });
            }
            let hr = codecs(&mut count);
            if hr != OK {
                return Err(Error::hr("Cannot enumerate codecs", hr));
            }
            let mut methods = Vec::new();
            for i in 0..count {
                methods.push(Codec {
                    id: api.property(|v| codec_prop(i, 0, v))?.number(),
                    name: api
                        .property(|v| codec_prop(i, 1, v))?
                        .text()
                        .unwrap_or_default(),
                    decoder: api
                        .property(|v| codec_prop(i, 7, v))?
                        .boolean()
                        .unwrap_or(false),
                    encoder: api
                        .property(|v| codec_prop(i, 8, v))?
                        .boolean()
                        .unwrap_or(false),
                });
            }
            let info = RuntimeInfo {
                version: "26.06".into(),
                formats: handlers.iter().map(|h| h.format.clone()).collect(),
                codecs: methods,
            };
            Ok(Self {
                api,
                info,
                handlers: Arc::new(handlers),
            })
        }
    }
    pub fn info(&self) -> RuntimeInfo {
        self.info.clone()
    }
    pub fn open(
        &self,
        input: Box<dyn ReadSeek + Send>,
        options: OpenOptions,
        control: Arc<dyn Control>,
    ) -> Result<Session, Error> {
        let shared = Arc::new(Shared::new(control));
        let source = Arc::new(Mutex::new(input));
        let extension = Path::new(&options.name)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("");
        let mut candidates = Vec::new();
        for handler in self.handlers.iter() {
            let hinted = options.format.as_ref().is_some_and(|format| {
                handler.format.name.eq_ignore_ascii_case(format)
                    || (format.eq_ignore_ascii_case("rar")
                        && handler.format.name.eq_ignore_ascii_case("rar5"))
            });
            let ext = handler
                .format
                .extensions
                .iter()
                .any(|e| e.eq_ignore_ascii_case(extension));
            let mut matched = false;
            for signature in signatures(handler) {
                let mut reader = source
                    .lock()
                    .map_err(|_| Error::message("Input lock poisoned"))?;
                reader
                    .seek(SeekFrom::Start(handler.signature_offset))
                    .map_err(|e| {
                        shared.record("seek", None, None, e);
                        shared.error(
                            "Cannot inspect archive signature",
                            FAIL,
                            ArchiveFlags::default(),
                        )
                    })?;
                let mut bytes = vec![0; signature.len()];
                match reader.read_exact(&mut bytes) {
                    Ok(()) => matched |= bytes == signature,
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {}
                    Err(e) => {
                        shared.record("read", None, None, e);
                        return Err(shared.error(
                            "Cannot inspect archive signature",
                            FAIL,
                            ArchiveFlags::default(),
                        ));
                    }
                }
            }
            if !hinted && !ext && handler.format.flags & ((1 << 12) | (1 << 13)) != 0 {
                continue;
            }
            candidates.push((
                if matched {
                    0
                } else if hinted {
                    1
                } else if ext {
                    2
                } else {
                    3
                },
                handler,
            ));
        }
        candidates.sort_by_key(|(priority, _)| *priority);
        let mut best = None;
        for (priority, handler) in candidates {
            if shared.cancelled() {
                return Err(shared.error("Archive open cancelled", ABORT, ArchiveFlags::default()));
            }
            source
                .lock()
                .map_err(|_| Error::message("Input lock poisoned"))?
                .seek(SeekFrom::Start(0))
                .map_err(|e| {
                    shared.record("seek", None, None, e);
                    shared.error("Cannot rewind archive input", FAIL, ArchiveFlags::default())
                })?;
            let mut raw = ptr::null_mut();
            let hr = unsafe { (self.api.create)(&handler.class, &iid(6, 0x60), &mut raw) };
            if hr != OK || raw.is_null() {
                return Err(Error::hr(
                    "Cannot instantiate archive handler",
                    if hr == OK { FAIL } else { hr },
                ));
            }
            let archive = Com(raw);
            let input = callbacks::input(source.clone(), shared.clone());
            let (callback, _) =
                Callback::new(self.api.clone(), options.clone(), shared.clone(), None);
            let mut session = Session {
                archive,
                input,
                open_callback: callback,
                api: self.api.clone(),
                shared: shared.clone(),
                options: options.clone(),
                format: handler.format.name.clone(),
                open_stream_errors: Vec::new(),
                _thread: PhantomData,
            };
            let hr = unsafe {
                (session.archive.table::<ArchiveVtable>().open)(
                    session.archive.0,
                    session.input.0,
                    ptr::null(),
                    session.open_callback.0,
                )
            };
            if hr == OK {
                session.open_stream_errors = shared.errors();
                return Ok(session);
            }
            let flags = session.flags().unwrap_or_default();
            let error = shared.error(
                format!("Cannot open archive as {}", handler.format.name),
                hr,
                flags,
            );
            if error.password_requested || !error.stream_errors.is_empty() || shared.cancelled() {
                return Err(error);
            }
            if best.is_none() || priority < 3 {
                best = Some(error);
            }
        }
        Err(best.unwrap_or_else(|| Error::message("No matching archive handler")))
    }
}
fn signatures(handler: &Handler) -> Vec<&[u8]> {
    let mut result = Vec::new();
    if !handler.signature.is_empty() {
        result.push(handler.signature.as_slice());
    }
    let mut data = handler.multi_signature.as_slice();
    while let Some((&len, tail)) = data.split_first() {
        let n = len as usize;
        if n > tail.len() {
            break;
        }
        if n > 0 {
            result.push(&tail[..n]);
        }
        data = &tail[n..];
    }
    result
}

pub struct Session {
    archive: Com,
    input: Com,
    open_callback: Com,
    api: Arc<Api>,
    shared: Arc<Shared>,
    options: OpenOptions,
    format: String,
    open_stream_errors: Vec<StreamError>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            (self.archive.table::<ArchiveVtable>().close)(self.archive.0);
        }
    }
}
impl Session {
    pub fn format(&self) -> &str {
        &self.format
    }
    fn property(&self, index: u32, id: u32) -> Result<PropertyValue<'_>, Error> {
        self.api.property(|v| unsafe {
            (self.archive.table::<ArchiveVtable>().property)(self.archive.0, index, id, v)
        })
    }
    fn flags(&self) -> Result<ArchiveFlags, Error> {
        let get = |id| {
            self.api
                .property(|v| unsafe {
                    (self.archive.table::<ArchiveVtable>().archive_property)(self.archive.0, id, v)
                })
                .map(|p| p.number().map(|n| n as u32))
        };
        Ok(ArchiveFlags {
            errors: get(71)?,
            warnings: get(72)?,
        })
    }
    pub fn entries(&mut self) -> Result<Vec<Entry>, Error> {
        let mut count = 0;
        let hr =
            unsafe { (self.archive.table::<ArchiveVtable>().count)(self.archive.0, &mut count) };
        if hr != OK {
            return Err(self.shared.error(
                "Cannot enumerate archive entries",
                hr,
                self.flags().unwrap_or_default(),
            ));
        }
        let mut entries = Vec::new();
        for index in 0..count {
            entries.push(Entry {
                index,
                name: self
                    .property(index, 3)?
                    .text()
                    .or(self.property(index, 4)?.text())
                    .unwrap_or_default(),
                is_dir: self.property(index, 6)?.boolean().unwrap_or(false),
                size: self.property(index, 7)?.number(),
                packed_size: self.property(index, 8)?.number(),
                encrypted: self.property(index, 15)?.boolean(),
                link: self.property(index, 54)?.text(),
                hard_link: self.property(index, 90)?.text(),
                unix_mode: self.property(index, 53)?.number().map(|n| n as u32),
                modified: self.property(index, 12)?.filetime(),
                method: self.property(index, 22)?.text(),
                crc: self.property(index, 19)?.number().map(|n| n as u32),
                block: self.property(index, 27)?.number(),
            });
        }
        Ok(entries)
    }
    pub fn run(
        &mut self,
        indices: Option<&[u32]>,
        test: bool,
        factory: Arc<dyn OutputFactory>,
        control: Arc<dyn Control>,
    ) -> Result<RunReport, Error> {
        if let Some(indices) = indices {
            if indices.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(Error::message(
                    "Archive indices must be strictly increasing",
                ));
            }
            if indices.len() >= u32::MAX as usize {
                return Err(Error::message("Too many archive indices"));
            }
        }
        self.shared.reset(control);
        let (callback, state) = Callback::new(
            self.api.clone(),
            self.options.clone(),
            self.shared.clone(),
            Some(factory),
        );
        let hr = unsafe {
            (self.archive.table::<ArchiveVtable>().extract)(
                self.archive.0,
                indices.map_or(ptr::null(), |v| v.as_ptr()),
                indices.map_or(u32::MAX, |v| v.len() as u32),
                test as i32,
                callback.0,
            )
        };
        // COM calls have returned; callback/output references are released before report delivery.
        let (items, extra_results) = unsafe { Callback::results(state) };
        drop(callback);
        let flags = self.flags()?;
        Ok(RunReport {
            hresult: hr,
            items,
            extra_results,
            flags,
            password_requested: self.shared.password_requested(),
            stream_errors: self
                .open_stream_errors
                .iter()
                .cloned()
                .chain(self.shared.errors())
                .collect(),
        })
    }
}
