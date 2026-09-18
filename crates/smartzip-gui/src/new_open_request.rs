#[cfg(target_os = "macos")]
use std::io::Read;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

const MAX_QUICK_PATHS: usize = 1_000;
const MAX_QUICK_URL_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenIntent {
    Preview,
    QuickExtract,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenRequest {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) intent: OpenIntent,
}

pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> OpenRequest {
    let mut args = args.into_iter().peekable();
    let intent = if args
        .peek()
        .is_some_and(|arg| arg == std::ffi::OsStr::new("--quick-extract"))
    {
        args.next();
        OpenIntent::QuickExtract
    } else {
        OpenIntent::Preview
    };
    if args
        .peek()
        .is_some_and(|arg| arg == std::ffi::OsStr::new("--"))
    {
        args.next();
    }
    let paths = args.map(PathBuf::from).collect();
    OpenRequest { paths, intent }
}

pub(crate) fn path_from_url(raw_url: &str) -> Option<PathBuf> {
    let url = url::Url::parse(raw_url).ok()?;
    if url.scheme() != "file"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || matches!(url.host_str(), Some(host) if !host.eq_ignore_ascii_case("localhost"))
    {
        return None;
    }
    url.to_file_path().ok()
}

fn smartzip_request_from_url(url: &url::Url, expected_token: Option<&str>) -> Option<OpenRequest> {
    if url.scheme() != "smartzip"
        || url.host_str() != Some("extract")
        || url.path() != ""
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    if url.as_str().len() > MAX_QUICK_URL_BYTES {
        return None;
    }
    let query = url.query()?;
    let mut paths = Vec::new();
    let mut token_seen = false;
    for pair in query.split('&') {
        let (key, _) = pair.split_once('=')?;
        if key != "path" && key != "token" {
            return None;
        }
        let (key, value) = url::form_urlencoded::parse(pair.as_bytes()).next()?;
        if key == "token" {
            if token_seen {
                return None;
            }
            token_seen = true;
            if value != expected_token? {
                return None;
            }
        } else {
            let path = PathBuf::from(value.into_owned());
            let path_string = path.to_str()?;
            if path_string.is_empty() || path_string.contains('\0') || !path.is_absolute() {
                return None;
            }
            paths.push(path);
            if paths.len() > MAX_QUICK_PATHS {
                return None;
            }
        }
    }
    if !paths.is_empty() && token_seen && expected_token.is_some() {
        Some(OpenRequest {
            paths,
            intent: OpenIntent::QuickExtract,
        })
    } else {
        None
    }
}

pub(crate) fn request_from_url(raw_url: &str, expected_token: Option<&str>) -> Option<OpenRequest> {
    let url = url::Url::parse(raw_url).ok()?;
    if url.scheme() == "file" {
        return Some(OpenRequest {
            paths: vec![path_from_url(raw_url)?],
            intent: OpenIntent::Preview,
        });
    }
    smartzip_request_from_url(&url, expected_token)
}

pub(crate) fn requests_from_urls(urls: impl IntoIterator<Item = String>) -> Vec<OpenRequest> {
    let urls: Vec<String> = urls.into_iter().collect();
    let expected_token = if urls.iter().any(|raw_url| {
        url::Url::parse(raw_url)
            .ok()
            .is_some_and(|url| url.scheme() == "smartzip")
    }) {
        finder_token()
    } else {
        None
    };
    requests_with_token(urls, expected_token.as_deref())
}

fn requests_with_token(
    urls: impl IntoIterator<Item = String>,
    expected_token: Option<&str>,
) -> Vec<OpenRequest> {
    urls.into_iter()
        .filter_map(|url| request_from_url(&url, expected_token))
        .collect()
}

pub(crate) fn smartzip_extract_url(paths: &[PathBuf], token: &str) -> Option<String> {
    if paths.is_empty() || paths.len() > MAX_QUICK_PATHS {
        return None;
    }
    let mut url = url::Url::parse("smartzip://extract").ok()?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("token", token);
        for path in paths {
            let path = path.to_str()?;
            if path.is_empty() || path.contains('\0') || !Path::new(path).is_absolute() {
                return None;
            }
            query.append_pair("path", path);
        }
    }
    let url: String = url.into();
    (url.len() <= MAX_QUICK_URL_BYTES).then_some(url)
}

pub(crate) fn forward_finder_quick_extract(paths: &[OsString]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("--finder-quick-extract requires at least one file".into());
    }
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let token = finder_token().ok_or("Finder quick extract is unsupported on this platform")?;
    let url = smartzip_extract_url(&paths, &token).ok_or("could not construct SmartZip URL")?;
    forward_finder_url(&url)
}

#[cfg(target_os = "macos")]
fn finder_token() -> Option<String> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = smartzip_platform::PlatformPaths::try_new()
        .ok()?
        .data_dir
        .join("finder-open-token");
    std::fs::create_dir_all(path.parent()?).ok()?;
    if let Ok(token) = read_token(&path) {
        return Some(token);
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut bytes)
        .ok()?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(token.as_bytes()).ok()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return read_token(&path).ok();
        }
        Err(_) => return None,
    }
    read_token(&path).ok()
}

#[cfg(target_os = "macos")]
fn read_token(path: &Path) -> std::io::Result<String> {
    let metadata = std::fs::symlink_metadata(path)?;
    use std::os::unix::fs::PermissionsExt;
    if !metadata.file_type().is_file()
        || metadata.len() != 64
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::other("invalid private token file"));
    }
    let bytes = std::fs::read(path)?;
    let token = std::str::from_utf8(&bytes)
        .map_err(std::io::Error::other)?
        .to_owned();
    if bytes.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(std::io::Error::other("invalid token"));
    }
    Ok(token)
}

#[cfg(not(target_os = "macos"))]
fn finder_token() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn forward_finder_url(url: &str) -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/open")
        .args(["-b", "org.smartzip.SmartZip", url])
        .status()
        .map_err(|e| format!("failed to invoke Finder handoff: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Finder handoff exited with {status}"))
    }
}

#[cfg(not(target_os = "macos"))]
fn forward_finder_url(_url: &str) -> Result<(), String> {
    Err("Finder quick extract is unsupported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_quick_extract_only_as_first_flag() {
        let r = parse_args([
            OsString::from("--quick-extract"),
            OsString::from("--"),
            OsString::from("-archive.zip"),
        ]);
        assert_eq!(r.intent, OpenIntent::QuickExtract);
        assert_eq!(r.paths, vec![PathBuf::from("-archive.zip")]);
        let r = parse_args([
            OsString::from("archive.zip"),
            OsString::from("--quick-extract"),
        ]);
        assert_eq!(r.intent, OpenIntent::Preview);
        assert_eq!(r.paths[1], PathBuf::from("--quick-extract"));
        let r = parse_args([
            OsString::from("--"),
            OsString::from("a.zip"),
            OsString::from("--"),
        ]);
        assert_eq!(r.paths, vec![PathBuf::from("a.zip"), PathBuf::from("--")]);
    }
    #[test]
    fn file_url_decodes_unicode_and_rejects_remote() {
        assert_eq!(
            request_from_url("file:///tmp/%E4%B8%AD%E6%96%87%20archive.zip", None)
                .unwrap()
                .paths[0],
            PathBuf::from("/tmp/中文 archive.zip")
        );
        assert_eq!(request_from_url("file://server/archive.zip", None), None);
        assert_eq!(request_from_url("file:///tmp/archive.zip?x=1", None), None);
        assert_eq!(
            request_from_url("https://example.com/archive.zip", None),
            None
        );
    }
    #[test]
    fn smartzip_url_roundtrips_multiple_paths() {
        let paths = vec![
            PathBuf::from("/tmp/中文 archive.zip"),
            PathBuf::from("/tmp/a&b.zip"),
        ];
        let url = smartzip_extract_url(&paths, "test-token").unwrap();
        assert_eq!(
            request_from_url(&url, Some("test-token")),
            Some(OpenRequest {
                paths,
                intent: OpenIntent::QuickExtract
            })
        );
    }
    #[test]
    fn smartzip_url_rejects_unlisted_parts() {
        assert_eq!(
            request_from_url(
                "smartzip://extract/foo?token=test-token&path=%2Ftmp%2Fa.zip",
                Some("test-token")
            ),
            None
        );
        assert_eq!(
            request_from_url(
                "smartzip://other?token=test-token&path=%2Ftmp%2Fa.zip",
                Some("test-token")
            ),
            None
        );
        assert_eq!(
            request_from_url(
                "smartzip://extract?token=test-token&path=%2Ftmp%2Fa.zip&x=y",
                Some("test-token")
            ),
            None
        );
        assert_eq!(
            request_from_url(
                "smartzip://extract?token=test-token&path=relative.zip",
                Some("test-token")
            ),
            None
        );
    }

    #[test]
    fn keeps_mixed_url_intents_separate() {
        let requests = requests_with_token(
            [
                "file:///tmp/a.zip".into(),
                "smartzip://extract?token=test-token&path=%2Ftmp%2Fb.zip".into(),
            ],
            Some("test-token"),
        );
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].intent, OpenIntent::Preview);
        assert_eq!(requests[1].intent, OpenIntent::QuickExtract);
    }
}
