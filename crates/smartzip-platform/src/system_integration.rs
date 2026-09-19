//! User-controlled document associations. Launching the app never changes defaults.
use serde::Deserialize;
use std::{
    io,
    path::{Path, PathBuf},
};

pub const BUNDLE_ID: &str = "org.smartzip.SmartZip";
#[derive(Clone, Debug, Deserialize)]
pub struct ArchiveType {
    pub label: String,
    pub extension: String,
    pub uti: String,
    pub mime: String,
}
pub fn archive_types() -> Vec<ArchiveType> {
    serde_json::from_str(include_str!("../../../resources/file-types.json"))
        .expect("embedded archive type manifest")
}
#[derive(Clone, Debug)]
pub struct Association {
    pub format: ArchiveType,
    pub application: Option<PathBuf>,
    pub is_default: bool,
}
#[derive(Clone, Debug)]
pub struct IntegrationStatus {
    pub bundle: Option<PathBuf>,
    pub associations: Vec<Association>,
    pub finder_installed: bool,
}
pub fn validate_bundle(path: &Path) -> io::Result<()> {
    if !path.is_dir() || path.is_symlink() {
        return Err(io::Error::other("请选择实际的 SmartZip.app 应用"));
    }
    let info =
        plist::Value::from_file(path.join("Contents/Info.plist")).map_err(io::Error::other)?;
    if info
        .as_dictionary()
        .and_then(|d| d.get("CFBundleIdentifier"))
        .and_then(plist::Value::as_string)
        != Some(BUNDLE_ID)
        || !path.join("Contents/MacOS/smartzip-gui").is_file()
    {
        return Err(io::Error::other("应用标识或可执行文件不匹配"));
    }
    Ok(())
}
pub fn current_bundle() -> io::Result<Option<PathBuf>> {
    let executable = std::env::current_exe()?;
    for path in executable.ancestors() {
        if path.extension().is_some_and(|extension| extension == "app") {
            validate_bundle(path)?;
            return Ok(Some(path.into()));
        }
    }
    Ok(None)
}
fn required_bundle() -> io::Result<PathBuf> {
    current_bundle()?.ok_or_else(|| {
        io::Error::other("请使用打包后的 SmartZip.app，并将它放在固定位置后配置系统集成")
    })
}
fn supported(extension: &str) -> io::Result<()> {
    if archive_types()
        .iter()
        .any(|format| format.extension == extension)
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "不支持此文件类型关联",
        ))
    }
}
#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use core_foundation::{
        base::TCFType,
        url::{CFURLRef, CFURL},
    };
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSError, NSString, NSURL};
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSRegisterURL(url: CFURLRef, update: u8) -> i32;
    }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        fn NSUpdateDynamicServices();
    }
    pub fn refresh_services() {
        // Notify AppKit after this user-requested service install/removal.
        unsafe {
            NSUpdateDynamicServices();
        }
    }
    pub fn register(bundle: &Path) -> io::Result<()> {
        validate_bundle(bundle)?;
        let url = CFURL::from_path(bundle, true).ok_or_else(|| io::Error::other("应用路径无效"))?;
        // CFURL remains alive through the synchronous Launch Services call.
        let result = unsafe { LSRegisterURL(url.as_concrete_TypeRef(), 1) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!("系统注册失败：{result}")))
        }
    }
    pub fn status() -> io::Result<IntegrationStatus> {
        let bundle = current_bundle()?;
        let probe = tempfile::tempdir()?;
        let mut associations = Vec::new();
        for format in archive_types() {
            let path = probe.path().join(format!("archive.{}", format.extension));
            std::fs::write(&path, [])?;
            // NSWorkspace resolves type from the harmless temporary file URL.
            let application = unsafe {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                NSWorkspace::sharedWorkspace()
                    .URLForApplicationToOpenURL(&url)
                    .and_then(|url| url.path())
                    .map(|path| PathBuf::from(path.to_string()))
            };
            let is_default = application
                .as_ref()
                .is_some_and(|app| validate_bundle(app).is_ok());
            associations.push(Association {
                format,
                application,
                is_default,
            });
        }
        Ok(IntegrationStatus {
            bundle,
            associations,
            finder_installed: crate::finder_service::installed()?,
        })
    }
    pub fn set_default(
        extension: &str,
        done: Box<dyn Fn(Result<(), String>) + Send + Sync>,
    ) -> io::Result<()> {
        supported(extension)?;
        let bundle = required_bundle()?;
        register(&bundle)?;
        let probe = std::sync::Arc::new(tempfile::tempdir()?);
        let path = probe.path().join(format!("archive.{extension}"));
        std::fs::write(&path, [])?;
        let handler = block2::RcBlock::new(move |error: *mut NSError| {
            let _keep_alive = &probe;
            // NSError is supplied by AppKit and valid for this callback.
            let result = if error.is_null() {
                Ok(())
            } else {
                Err(unsafe { (&*error).localizedDescription() }.to_string())
            };
            done(result);
        });
        // The OS owns a copy of the completion block and presents consent if needed.
        unsafe {
            let app_url = NSURL::fileURLWithPath(&NSString::from_str(&bundle.to_string_lossy()));
            let type_url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            NSWorkspace::sharedWorkspace()
                .setDefaultApplicationAtURL_toOpenContentTypeOfFileAtURL_completionHandler(
                    &app_url,
                    &type_url,
                    Some(&handler),
                );
        }
        Ok(())
    }
}
#[cfg(not(target_os = "macos"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "当前系统尚未接入默认程序和右键菜单管理",
    )
}
pub fn status() -> io::Result<IntegrationStatus> {
    #[cfg(target_os = "macos")]
    {
        mac::status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported())
    }
}
pub fn register_application() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let bundle = required_bundle()?;
        mac::register(&bundle)?;
        Ok(bundle)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unsupported())
    }
}
pub fn set_default(
    extension: &str,
    done: Box<dyn Fn(Result<(), String>) + Send + Sync>,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::set_default(extension, done)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (extension, done);
        Err(unsupported())
    }
}
pub fn install_finder_service() -> io::Result<PathBuf> {
    let bundle = register_application()?;
    let path = crate::finder_service::install(&bundle)?;
    #[cfg(target_os = "macos")]
    mac::refresh_services();
    Ok(path)
}
pub fn remove_finder_service() -> io::Result<()> {
    crate::finder_service::remove()?;
    #[cfg(target_os = "macos")]
    mac::refresh_services();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn association_manifest_is_explicit_and_does_not_claim_executables() {
        let formats = archive_types();
        assert_eq!(formats.len(), 8);
        for extension in ["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst"] {
            assert!(supported(extension).is_ok());
        }
        for extension in ["exe", "app", "001", "*"] {
            assert!(supported(extension).is_err());
        }
    }
    #[test]
    fn unrelated_bundle_cannot_register_as_smartzip() {
        let root = tempfile::tempdir().unwrap();
        assert!(validate_bundle(root.path()).is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn native_status_reads_associations_without_changing_them() {
        let snapshot = status().unwrap();
        assert_eq!(snapshot.associations.len(), archive_types().len());
        assert!(snapshot.bundle.is_none()); // cargo test is not an installed .app.
    }
}
