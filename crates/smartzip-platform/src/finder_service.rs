//! Installation of the macOS Finder Quick Action for extracting archives.

use plist::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const SERVICE_NAME: &str = "SmartZip 快速解压.workflow";
const OWNER_MARKER: &str = ".smartzip-owner";
const OWNER_VALUE: &str = "SmartZip Finder Quick Action";

#[cfg(not(target_os = "macos"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Finder Quick Actions are only supported on macOS",
    )
}

/// Return the path of the SmartZip Finder Quick Action.
pub fn service_path() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = directories::BaseDirs::new()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory unavailable"))?;
        return Ok(home.home_dir().join("Library/Services").join(SERVICE_NAME));
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported())
    }
}

/// Install the Quick Action, returning its final path.
pub fn install(bundle: &Path) -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let destination = service_path()?;
        if destination.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to replace existing {}", destination.display()),
            ));
        }
        let services = destination
            .parent()
            .ok_or_else(|| io::Error::other("invalid Services path"))?;
        fs::create_dir_all(services)?;
        let temporary = tempfile::Builder::new()
            .prefix(".smartzip-finder-")
            .tempdir_in(services)?;
        let temporary_path = temporary.path().to_path_buf();
        let result = (|| {
            let contents = temporary_path.join("Contents");
            fs::create_dir_all(&contents)?;
            write_plist(&contents.join("document.wflow"), workflow_document(bundle))?;
            write_plist(&contents.join("Info.plist"), info_plist())?;
            fs::write(
                contents.join(OWNER_MARKER),
                owner_manifest(
                    &contents.join("document.wflow"),
                    &contents.join("Info.plist"),
                )?,
            )?;
            fs::rename(&temporary_path, &destination)?;
            Ok(destination.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&temporary_path);
        }
        result
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle;
        Err(unsupported())
    }
}

/// Report whether the generated Quick Action is installed.
pub fn installed() -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        let path = service_path()?;
        Ok(path.is_dir() && owns_service(&path))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported())
    }
}

/// Remove the generated Quick Action without replacing or deleting an unknown service.
pub fn remove() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let path = service_path()?;
        if !path.try_exists()? {
            return Ok(());
        }
        if !owns_service(&path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to remove an unowned Finder Quick Action",
            ));
        }
        fs::remove_dir_all(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported())
    }
}

fn owns_service(path: &Path) -> bool {
    if !path.is_dir() || path.is_symlink() {
        return false;
    }
    let contents = path.join("Contents");
    if !contents.is_dir() || contents.is_symlink() {
        return false;
    }
    let entries = match fs::read_dir(&contents) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return false,
        };
        if entry.path().is_symlink() {
            return false;
        }
        names.push(entry.file_name());
    }
    names.sort();
    if names
        != [
            std::ffi::OsString::from(OWNER_MARKER),
            std::ffi::OsString::from("Info.plist"),
            std::ffi::OsString::from("document.wflow"),
        ]
    {
        return false;
    }
    let marker = match fs::read_to_string(contents.join(OWNER_MARKER)) {
        Ok(marker) => marker,
        Err(_) => return false,
    };
    let expected = format!(
        "{OWNER_VALUE}\ndocument.wflow={}\nInfo.plist={}\n",
        content_hash(&contents.join("document.wflow")),
        content_hash(&contents.join("Info.plist")),
    );
    marker == expected
}

fn content_hash(path: &Path) -> String {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return String::new(),
    };
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn owner_manifest(document: &Path, info: &Path) -> io::Result<String> {
    let document_hash = content_hash(document);
    let info_hash = content_hash(info);
    if document_hash.is_empty() || info_hash.is_empty() {
        return Err(io::Error::other("unable to fingerprint generated workflow"));
    }
    Ok(format!(
        "{OWNER_VALUE}\ndocument.wflow={document_hash}\nInfo.plist={info_hash}\n"
    ))
}

fn write_plist(path: &Path, value: Value) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    plist::to_writer_xml(&mut file, &value).map_err(io::Error::other)
}

fn dict(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(
        entries
            .into_iter()
            .map(|(key, value)| (String::from(key), value))
            .collect(),
    )
}

fn shell_quote(value: &Path) -> String {
    let value = value.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn workflow_document(bundle: &Path) -> Value {
    let executable = bundle.join("Contents/MacOS/smartzip-gui");
    let command = format!(
        "exec {} --finder-quick-extract \"$@\"",
        shell_quote(&executable)
    );
    let action = dict([
        (
            "action",
            dict([
                (
                    "ActionParameters",
                    dict([
                        ("COMMAND_STRING", Value::String(command)),
                        ("inputMethod", Value::Integer(1.into())),
                        ("shell", Value::String("/bin/sh".into())),
                    ]),
                ),
                (
                    "BundleIdentifier",
                    Value::String("com.apple.RunShellScript".into()),
                ),
                (
                    "ActionBundlePath",
                    Value::String("/System/Library/Automator/Run Shell Script.action".into()),
                ),
                ("ActionName", Value::String("Run Shell Script".into())),
            ]),
        ),
        ("isViewVisible", Value::Integer(1.into())),
    ]);
    dict([
        ("AMDocumentVersion", Value::String("2".into())),
        ("actions", Value::Array(vec![action])),
        ("connectors", dict([])),
        (
            "workflowMetaData",
            dict([
                (
                    "applicationBundleID",
                    Value::String("com.apple.finder".into()),
                ),
                (
                    "inputTypeIdentifier",
                    Value::String("com.apple.Automator.fileSystemObject".into()),
                ),
                (
                    "workflowTypeIdentifier",
                    Value::String("com.apple.Automator.servicesMenu".into()),
                ),
                ("presentationMode", Value::Integer(15.into())),
            ]),
        ),
    ])
}

fn info_plist() -> Value {
    dict([(
        "NSServices",
        Value::Array(vec![dict([
            ("NSMessage", Value::String("runWorkflowAsService".into())),
            (
                "NSMenuItem",
                dict([("default", Value::String("SmartZip 快速解压".into()))]),
            ),
            (
                "NSSendFileTypes",
                Value::Array(vec![Value::String("public.item".into())]),
            ),
            (
                "NSRequiredContext",
                dict([(
                    "NSApplicationIdentifier",
                    Value::String("com.apple.finder".into()),
                )]),
            ),
        ])]),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_quotes_bundle_paths_and_arguments() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workflow.plist");
        write_plist(
            &path,
            workflow_document(Path::new("/tmp/a path/it's SmartZip.app")),
        )
        .unwrap();
        let value = Value::from_file(&path).unwrap();
        let action = &value
            .as_dictionary()
            .unwrap()
            .get("actions")
            .unwrap()
            .as_array()
            .unwrap()[0];
        let action = action
            .as_dictionary()
            .unwrap()
            .get("action")
            .unwrap()
            .as_dictionary()
            .unwrap();
        assert_eq!(
            action.get("BundleIdentifier").unwrap().as_string(),
            Some("com.apple.RunShellScript")
        );
        let parameters = action
            .get("ActionParameters")
            .unwrap()
            .as_dictionary()
            .unwrap();
        assert_eq!(
            parameters.get("inputMethod").unwrap().as_signed_integer(),
            Some(1)
        );
        assert_eq!(
            parameters.get("shell").unwrap().as_string(),
            Some("/bin/sh")
        );
        let command = parameters
            .get("COMMAND_STRING")
            .unwrap()
            .as_string()
            .unwrap();
        assert!(command.contains("'/tmp/a path/it'\\''s SmartZip.app/Contents/MacOS/smartzip-gui'"));
        assert!(command.contains("--finder-quick-extract \"$@\""));
    }

    #[test]
    fn owner_marker_distinguishes_generated_bundle() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join(SERVICE_NAME);
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        assert!(!owns_service(&bundle));
        let contents = bundle.join("Contents");
        fs::write(contents.join("document.wflow"), b"workflow").unwrap();
        fs::write(contents.join("Info.plist"), b"info").unwrap();
        fs::write(
            contents.join(OWNER_MARKER),
            owner_manifest(
                &contents.join("document.wflow"),
                &contents.join("Info.plist"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(owns_service(&bundle));
        fs::write(contents.join("Info.plist"), b"user-owned\n").unwrap();
        assert!(!owns_service(&bundle));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn automator_runs_generated_workflow_with_file_arguments() {
        if !Path::new("/usr/bin/automator").is_file() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("SmartZip app's.app");
        let executable = app.join("Contents/MacOS/smartzip-gui");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let log = root.path().join("arguments.log");
        fs::write(
            &executable,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", shell_quote(&log)),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

        let workflow = root.path().join("SmartZip.workflow/Contents");
        fs::create_dir_all(&workflow).unwrap();
        write_plist(&workflow.join("document.wflow"), workflow_document(&app)).unwrap();
        let input = root.path().join("archive file's.zip");
        fs::write(&input, b"test").unwrap();
        let status = std::process::Command::new("/usr/bin/automator")
            .args([
                "-i",
                input.to_str().unwrap(),
                workflow.join("document.wflow").to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            format!("--finder-quick-extract\n{}\n", input.display())
        );
    }
}
