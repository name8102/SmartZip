//! Local open-request forwarding, scoped to the selected desktop profile.
#[cfg(unix)]
mod unix {
    use crate::new_open_request::{OpenIntent, OpenRequest};
    use std::{
        fs::{File, OpenOptions},
        hash::{Hash, Hasher},
        io::{self, Read, Write},
        os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
        },
        path::{Path, PathBuf},
        sync::mpsc::Sender,
        time::{Duration, Instant},
    };
    const MAX_MESSAGE: u64 = 1024 * 1024;
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Message {
        paths: Vec<Vec<u8>>,
        quick: bool,
    }
    pub struct Owner {
        _lock: File,
        socket: PathBuf,
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket);
        }
    }
    fn encode(request: &OpenRequest) -> io::Result<Vec<u8>> {
        serde_json::to_vec(&Message {
            paths: request
                .paths
                .iter()
                .map(|p| p.as_os_str().as_bytes().to_vec())
                .collect(),
            quick: request.intent == OpenIntent::QuickExtract,
        })
        .map_err(io::Error::other)
    }
    fn receive(stream: &mut UnixStream) -> io::Result<OpenRequest> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut size = [0; 8];
        stream.read_exact(&mut size)?;
        let length = u64::from_be_bytes(size);
        if length > MAX_MESSAGE {
            return Err(io::Error::other("打开请求过大"));
        }
        let mut data = vec![0; length as usize];
        stream.read_exact(&mut data)?;
        let m: Message = serde_json::from_slice(&data).map_err(io::Error::other)?;
        if m.paths.len() > 1000 || m.paths.iter().any(|p| p.is_empty() || p.contains(&0)) {
            return Err(io::Error::other("打开请求路径无效"));
        }
        Ok(OpenRequest {
            paths: m
                .paths
                .into_iter()
                .map(|p| PathBuf::from(std::ffi::OsString::from_vec(p)))
                .collect(),
            intent: if m.quick {
                OpenIntent::QuickExtract
            } else {
                OpenIntent::Preview
            },
        })
    }
    pub fn claim(
        profile: &Path,
        request: &OpenRequest,
        sender: Sender<OpenRequest>,
    ) -> io::Result<Option<Owner>> {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        profile.hash(&mut hash);
        // Private directory and lock ownership prevent another account from replacing the socket.
        let uid = unsafe { libc::geteuid() };
        let dir = std::env::temp_dir().join(format!("smartzip-{uid}-{:x}", hash.finish()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let metadata = std::fs::symlink_metadata(&dir)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::other("实例目录权限无效"));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join("instance.lock"))?;
        let socket = dir.join("open.sock");
        match lock.try_lock() {
            Ok(()) => {
                match std::fs::remove_file(&socket) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
                let listener = UnixListener::bind(&socket)?;
                std::thread::spawn(move || {
                    for mut stream in listener.incoming().flatten() {
                        if let Ok(request) = receive(&mut stream) {
                            if sender.send(request).is_ok() {
                                let _ = stream.write_all(b"ok");
                            }
                        }
                    }
                });
                Ok(Some(Owner {
                    _lock: lock,
                    socket,
                }))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let data = encode(request)?;
                if data.len() as u64 > MAX_MESSAGE {
                    return Err(io::Error::other("打开请求过大"));
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut stream = loop {
                    match UnixStream::connect(&socket) {
                        Ok(s) => break s,
                        Err(e) if Instant::now() >= deadline => return Err(e),
                        Err(_) => std::thread::sleep(Duration::from_millis(20)),
                    }
                };
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(&(data.len() as u64).to_be_bytes())?;
                stream.write_all(&data)?;
                let mut ack = [0; 2];
                stream.read_exact(&mut ack)?;
                if ack != *b"ok" {
                    return Err(io::Error::other("已有实例未确认打开请求"));
                }
                Ok(None)
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn existing_instance_receives_literal_paths_and_intent() {
            let tmp = tempfile::tempdir().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let request = OpenRequest {
                paths: vec![
                    PathBuf::from("/tmp/中文 space $`%.zip"),
                    PathBuf::from(std::ffi::OsString::from_vec(
                        b"/tmp/nonutf8-\xff.zip".to_vec(),
                    )),
                ],
                intent: OpenIntent::QuickExtract,
            };
            let owner = claim(tmp.path(), &request, tx.clone()).unwrap().unwrap();
            assert!(claim(tmp.path(), &request, tx).unwrap().is_none());
            assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), request);
            drop(owner);
        }
    }
}
#[cfg(unix)]
pub use unix::claim;
