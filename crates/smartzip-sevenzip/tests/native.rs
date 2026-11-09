//! Explicit native acceptance checks. Fixtures are generated once; the product and
//! test execution do not invoke the 7-Zip CLI.
use smartzip_sevenzip::*;
use std::{
    collections::BTreeMap,
    io::{self, Cursor, Write},
    path::Path,
    sync::{Arc, Mutex},
};
const ZIP: &[u8] = include_bytes!("fixtures/plain.zip");
#[derive(Default)]
struct Progress;
impl Control for Progress {}
struct Cancel;
impl Control for Cancel {
    fn cancelled(&self) -> bool {
        true
    }
}
#[derive(Default)]
struct Output(Arc<Mutex<BTreeMap<u32, Vec<u8>>>>);
struct Writer {
    index: u32,
    output: Arc<Mutex<BTreeMap<u32, Vec<u8>>>>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output
            .lock()
            .unwrap()
            .entry(self.index)
            .or_default()
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl OutputFactory for Output {
    fn open(&self, index: u32) -> io::Result<Option<Box<dyn Write + Send>>> {
        Ok(Some(Box::new(Writer {
            index,
            output: self.0.clone(),
        })))
    }
}
struct BadOutput;
impl OutputFactory for BadOutput {
    fn open(&self, _: u32) -> io::Result<Option<Box<dyn Write + Send>>> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fixture output denied",
        ))
    }
}
struct PanicOutput;
impl OutputFactory for PanicOutput {
    fn open(&self, _: u32) -> io::Result<Option<Box<dyn Write + Send>>> {
        panic!("fixture callback panic")
    }
}
fn options(name: &str, password: Option<&str>) -> OpenOptions {
    OpenOptions {
        name: name.into(),
        password: password.map(str::to_owned),
        ..Default::default()
    }
}
fn open(
    library: &Library,
    bytes: &[u8],
    name: &str,
    password: Option<&str>,
) -> Result<Session, Error> {
    library.open(
        Box::new(Cursor::new(bytes.to_vec())),
        options(name, password),
        Arc::new(Progress),
    )
}
fn run(session: &mut Session, indices: Option<&[u32]>, test: bool) -> RunReport {
    session
        .run(
            indices,
            test,
            Arc::new(Output::default()),
            Arc::new(Progress),
        )
        .unwrap()
}
#[test]
#[ignore = "requires SMARTZIP_SEVENZIP_LIBRARY pointing to the official 26.06 native library"]
fn official_library_contract() {
    let path =
        std::env::var_os("SMARTZIP_SEVENZIP_LIBRARY").expect("set SMARTZIP_SEVENZIP_LIBRARY");
    let library = Library::load(Path::new(&path)).unwrap();
    assert_eq!(library.info().version, "26.06");
    assert!(library.info().formats.iter().any(|f| f.name == "Rar5"));
    assert!(library
        .info()
        .codecs
        .iter()
        .any(|c| c.decoder && c.name.starts_with("Rar")));
    let mut session = open(&library, ZIP, "misleading.bin", None).unwrap();
    assert_eq!(session.format(), "zip");
    let entries = session.entries().unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].name, "中文😀\n*.txt");
    assert_eq!(entries[1].name, entries[2].name);
    assert_eq!(entries[3].unix_mode.unwrap() & 0o170000, 0o120000);
    let sink = Arc::new(Output::default());
    let report = session
        .run(Some(&[2]), false, sink.clone(), Arc::new(Progress))
        .unwrap();
    assert_eq!(report.hresult, 0);
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].index, 2);
    assert_eq!(&sink.0.lock().unwrap()[&2], b"second");
    let report = run(&mut session, None, true);
    assert_eq!(report.hresult, 0);
    assert!(report.items.iter().all(|r| r.result == 0));
    let report = session
        .run(Some(&[0]), false, Arc::new(BadOutput), Arc::new(Progress))
        .unwrap();
    assert_ne!(report.hresult, 0);
    assert_eq!(report.stream_errors[0].kind, "PermissionDenied");
    let report = session
        .run(Some(&[0]), false, Arc::new(PanicOutput), Arc::new(Progress))
        .unwrap();
    assert_ne!(report.hresult, 0);
    assert!(report.stream_errors[0].message.contains("panicked"));
    let report = session
        .run(None, true, Arc::new(Output::default()), Arc::new(Cancel))
        .unwrap();
    assert_eq!(report.hresult, 0x80004004u32 as i32);
    let mut corrupted = open(
        &library,
        include_bytes!("fixtures/corrupt.zip"),
        "bad.zip",
        None,
    )
    .unwrap();
    let report = run(&mut corrupted, None, true);
    assert_eq!(report.hresult, 0);
    assert!(report.items.iter().any(|r| r.result == 3));
    for (name, bytes) in [
        ("plain.tar", include_bytes!("fixtures/plain.tar").as_slice()),
        ("plain.gz", include_bytes!("fixtures/plain.gz").as_slice()),
        ("plain.xz", include_bytes!("fixtures/plain.xz").as_slice()),
    ] {
        let mut session = open(&library, bytes, name, None).unwrap();
        let report = run(&mut session, None, true);
        assert_eq!(report.hresult, 0);
        assert!(!report.items.is_empty());
        assert!(report.items.iter().all(|r| r.result == 0));
    }
    let data = include_bytes!("fixtures/encrypted.7z");
    let mut session = open(&library, data, "data.7z", Some("秘密😀")).unwrap();
    let report = run(&mut session, None, true);
    assert_eq!(report.hresult, 0);
    assert!(report.password_requested);
    assert!(report.items.iter().all(|r| r.result == 0));
    let mut session = open(&library, data, "data.7z", Some("wrong")).unwrap();
    let report = run(&mut session, None, true);
    assert!(report.password_requested);
    assert!(report.items.iter().any(|r| r.result != 0));
    let header = include_bytes!("fixtures/headers.7z");
    let error = match open(&library, header, "headers.7z", None) {
        Err(e) => e,
        Ok(_) => panic!("encrypted header unexpectedly opened"),
    };
    assert!(error.password_requested);
    assert!(error.hresult.is_some());
    let error = match open(&library, header, "headers.7z", Some("wrong")) {
        Err(e) => e,
        Ok(_) => panic!("wrong header password accepted"),
    };
    assert!(error.password_requested);
    let mut session = open(&library, header, "headers.7z", Some("秘密😀")).unwrap();
    assert_eq!(run(&mut session, None, true).hresult, 0);
    // Unknown-extension SFX must search past a stub, not be constrained to offset zero.
    let mut sfx = vec![0x90; 257];
    sfx.extend_from_slice(include_bytes!("fixtures/encrypted.7z"));
    let mut session = open(&library, &sfx, "carrier.bin", Some("秘密😀")).unwrap();
    assert_eq!(session.format(), "7z");
    assert_eq!(run(&mut session, None, true).hresult, 0);
    // A session keeps the loaded code alive after the caller drops its Library handle.
    let mut session = open(&library, ZIP, "plain.zip", None).unwrap();
    drop(library);
    assert_eq!(run(&mut session, None, true).hresult, 0);
}

#[test]
#[ignore = "requires SMARTZIP_SEVENZIP_LIBRARY and SMARTZIP_RAR_VOLUME_DIR (RAR5 multipart fixture)"]
fn explicit_volume_callbacks() {
    let library = Library::load(Path::new(
        &std::env::var_os("SMARTZIP_SEVENZIP_LIBRARY").expect("set library path"),
    ))
    .unwrap();
    let directory = std::path::PathBuf::from(
        std::env::var_os("SMARTZIP_RAR_VOLUME_DIR").expect("set RAR volume fixture directory"),
    );
    let mut paths: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    paths.sort();
    let mut options = options(paths[0].file_name().unwrap().to_str().unwrap(), None);
    options.format = Some("rar".into());
    options.volumes = paths
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_str().unwrap().to_owned(),
                p.clone(),
            )
        })
        .collect();
    let mut session = library
        .open(
            Box::new(std::fs::File::open(&paths[0]).unwrap()),
            options.clone(),
            Arc::new(Progress),
        )
        .unwrap();
    let report = run(&mut session, None, true);
    assert_eq!(report.hresult, 0, "{report:?}");
    assert!(!report.items.is_empty());
    assert!(report.items.iter().all(|r| r.result == 0), "{report:?}");
    assert!(report.stream_errors.is_empty(), "{report:?}");
    options
        .volumes
        .remove(paths[1].file_name().unwrap().to_str().unwrap());
    let errors = match library.open(
        Box::new(std::fs::File::open(&paths[0]).unwrap()),
        options,
        Arc::new(Progress),
    ) {
        Ok(mut session) => run(&mut session, None, true).stream_errors,
        Err(error) => error.stream_errors,
    };
    assert!(
        errors
            .iter()
            .any(|e| e.operation == "volume" && e.kind == "NotFound"),
        "{errors:?}"
    );
}
