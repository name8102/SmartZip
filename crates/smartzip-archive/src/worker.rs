//! Private same-release worker protocol. Passwords travel only over stdin.
use serde::{Deserialize, Serialize};
use smartzip_sevenzip::{Control, Entry, Error, Library, OpenOptions, OutputFactory, RunReport};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Serialize, Deserialize)]
pub(crate) struct OsPath(Vec<u8>);
impl From<&std::path::Path> for OsPath {
    fn from(path: &std::path::Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self(
                path.as_os_str()
                    .encode_wide()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            )
        }
    }
}
impl OsPath {
    fn path(self) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(self.0).into()
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(
                &self
                    .0
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>(),
            )
            .into()
        }
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct WireInput {
    path: OsPath,
    offset: u64,
    length: Option<u64>,
    volumes: Vec<(String, OsPath)>,
}
impl From<&smartzip_core::ArchiveInput> for WireInput {
    fn from(i: &smartzip_core::ArchiveInput) -> Self {
        Self {
            path: (&*i.path).into(),
            offset: i.offset,
            length: i.length,
            volumes: i
                .volumes
                .iter()
                .map(|(n, p)| (n.clone(), p.as_path().into()))
                .collect(),
        }
    }
}
impl WireInput {
    fn input(self) -> smartzip_core::ArchiveInput {
        smartzip_core::ArchiveInput {
            path: self.path.path(),
            offset: self.offset,
            length: self.length,
            volumes: self
                .volumes
                .into_iter()
                .map(|(n, p)| (n, p.path()))
                .collect(),
        }
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Request {
    pub library: OsPath,
    pub input: WireInput,
    pub name: String,
    pub format: Option<String>,
    pub password: Option<String>,
    pub operation: Operation,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Operation {
    Inspect,
    Verify,
    Decode { indices: Option<Vec<u32>> },
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Event {
    Catalog { format: String, entries: Vec<Entry> },
    Progress { completed: u64, total: Option<u64> },
    Report(RunReport),
    Error(Error),
}
pub(crate) enum Frame {
    Event(Event),
    Data { index: u32, bytes: Vec<u8> },
    End(u32),
}

pub(crate) fn write_json(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    writer.write_all(&(data.len() as u64).to_le_bytes())?;
    writer.write_all(&data)?;
    writer.flush()
}
struct Pipe {
    output: Mutex<io::Stdout>,
    failed: AtomicBool,
    last_progress: Mutex<Option<std::time::Instant>>,
}
impl Pipe {
    fn event(&self, event: &Event) -> io::Result<()> {
        let mut out = self
            .output
            .lock()
            .map_err(|_| io::Error::other("protocol lock poisoned"))?;
        out.write_all(&[0])?;
        write_json(&mut *out, event)
    }
    fn data(&self, index: u32, bytes: &[u8]) -> io::Result<()> {
        let mut out = self
            .output
            .lock()
            .map_err(|_| io::Error::other("protocol lock poisoned"))?;
        out.write_all(&[1])?;
        out.write_all(&index.to_le_bytes())?;
        out.write_all(&(bytes.len() as u32).to_le_bytes())?;
        out.write_all(bytes)?;
        out.flush()
    }
}
impl Pipe {
    fn end(&self, index: u32) -> io::Result<()> {
        let mut out = self
            .output
            .lock()
            .map_err(|_| io::Error::other("protocol lock poisoned"))?;
        out.write_all(&[2])?;
        out.write_all(&index.to_le_bytes())?;
        out.flush()
    }
}
impl Control for Pipe {
    fn cancelled(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
    fn progress(&self, completed: u64, total: Option<u64>) {
        let publish = if let Ok(mut last) = self.last_progress.lock() {
            let now = std::time::Instant::now();
            if last.is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_millis(100))
                || total.is_some_and(|n| completed >= n)
            {
                *last = Some(now);
                true
            } else {
                false
            }
        } else {
            false
        };
        if !publish {
            return;
        }
        if self.event(&Event::Progress { completed, total }).is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
    }
}
struct Factory(Arc<Pipe>);
impl OutputFactory for Factory {
    fn open(&self, index: u32) -> io::Result<Option<Box<dyn Write + Send>>> {
        Ok(Some(Box::new(Stream {
            pipe: self.0.clone(),
            index,
        })))
    }
}
struct Stream {
    pipe: Arc<Pipe>,
    index: u32,
}
impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.pipe.end(self.index);
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for chunk in bytes.chunks(64 * 1024) {
            self.pipe.data(self.index, chunk)?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Worker entry point, not an application API.
pub fn run() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut length = [0; 8];
    input.read_exact(&mut length)?;
    let length = usize::try_from(u64::from_le_bytes(length)).map_err(io::Error::other)?;
    let mut data = vec![0; length];
    input.read_exact(&mut data)?;
    let request: Request = serde_json::from_slice(&data).map_err(io::Error::other)?;
    let pipe = Arc::new(Pipe {
        output: Mutex::new(io::stdout()),
        failed: AtomicBool::new(false),
        last_progress: Mutex::new(None),
    });
    let result = (|| {
        let library = Library::load(&request.library.path())?;
        let source = request.input.input();
        let stream = crate::source::InputReader::open(&source).map_err(|e| Error {
            message: e.to_string(),
            hresult: None,
            password_requested: false,
            flags: Default::default(),
            stream_errors: vec![],
        })?;
        let session_default_name = request.name.clone();
        let mut session = library.open(
            Box::new(stream),
            OpenOptions {
                format: request.format,
                name: request.name,
                password: request.password,
                volumes: source.volumes,
            },
            pipe.clone(),
        )?;
        let mut entries = session.entries()?;
        for entry in &mut entries {
            if entry.name.is_empty() {
                entry.name = std::path::Path::new(&session_default_name)
                    .file_stem()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(std::ffi::OsStr::new("data"))
                    .to_string_lossy()
                    .into_owned();
            }
        }
        pipe.event(&Event::Catalog {
            format: session.format().to_owned(),
            entries,
        })
        .map_err(|e| Error {
            message: e.to_string(),
            hresult: None,
            password_requested: false,
            flags: Default::default(),
            stream_errors: vec![],
        })?;
        let (indices, test) = match request.operation {
            Operation::Inspect => return Ok(None),
            Operation::Verify => (None, true),
            Operation::Decode { indices } => (indices, false),
        };
        let report = session.run(
            indices.as_deref(),
            test,
            Arc::new(Factory(pipe.clone())),
            pipe.clone(),
        )?;
        Ok(Some(report))
    })();
    match result {
        Ok(Some(report)) => pipe.event(&Event::Report(report)),
        Ok(None) => Ok(()),
        Err(error) => pipe.event(&Event::Error(error)),
    }
}
