use super::*;
use std::{io::Write, process::Command};
const DESKTOP_ID: &str = "org.smartzip.SmartZip.desktop";
fn launcher() -> io::Result<PathBuf> {
    let data = crate::PlatformPaths::try_new()?.data_dir;
    Ok(data
        .parent()
        .ok_or_else(|| io::Error::other("数据目录无效"))?
        .join("applications")
        .join(DESKTOP_ID))
}
// Match the installer: Exec has desktop-entry escaping, never shell interpolation.
fn quoted_executable(path: &Path) -> io::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::other("桌面启动器需要 UTF-8 可执行路径"))?;
    if value.contains(['\n', '\r', '\0']) {
        return Err(io::Error::other("桌面启动路径含不支持的控制字符"));
    }
    let mut value = value.replace('%', "%%");
    for character in ['\\', '"', '`', '$'] {
        value = value.replace(character, &format!("\\{character}"));
    }
    Ok(format!("\"{}\"", value.replace('\\', "\\\\")))
}
fn contents(executable: &Path) -> io::Result<String> {
    let exec = quoted_executable(executable)?;
    let mime = archive_types()
        .iter()
        .map(|f| f.mime.as_str())
        .collect::<Vec<_>>()
        .join(";");
    Ok(format!("[Desktop Entry]\nType=Application\nName=SmartZip\nComment=Extract and browse archives\nExec={exec} -- %F\nTerminal=false\nCategories=Utility;Archiving;\nMimeType={mime};\nActions=QuickExtract;\n\n[Desktop Action QuickExtract]\nName=Quick extract with SmartZip\nName[zh_CN]=使用 SmartZip 快速解压\nExec={exec} --quick-extract -- %F\n"))
}
pub(super) fn register() -> io::Result<PathBuf> {
    let path = launcher()?;
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    if path.is_symlink() {
        return Err(io::Error::other("拒绝替换符号链接启动器"));
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(contents(&std::env::current_exe()?)?.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|e| e.error)?;
    // Optional MIME cache refresh; setting defaults is a separate explicit operation.
    let _ = Command::new("update-desktop-database").arg(parent).output();
    Ok(path)
}
pub(super) fn status() -> io::Result<IntegrationStatus> {
    let launcher = launcher()?;
    let mut associations = Vec::new();
    for format in archive_types() {
        let output = Command::new("xdg-mime")
            .args(["query", "default", &format.mime])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("xdg-mime 无法读取默认打开方式"));
        }
        let app = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let is_default = app == DESKTOP_ID;
        associations.push(Association {
            format,
            application: (!app.is_empty()).then(|| PathBuf::from(app)),
            is_default,
        });
    }
    Ok(IntegrationStatus {
        bundle: launcher.is_file().then_some(launcher),
        associations,
        finder_installed: false,
    })
}
pub(super) fn set_default(
    extension: &str,
    done: Box<dyn Fn(Result<(), String>) + Send + Sync>,
) -> io::Result<()> {
    let format = archive_types()
        .into_iter()
        .find(|f| f.extension == extension)
        .ok_or_else(|| io::Error::other("不支持的归档类型"))?;
    if !launcher()?.is_file() {
        return Err(io::Error::other("请先注册打开方式"));
    }
    std::thread::spawn(move || {
        let result = Command::new("xdg-mime")
            .args(["default", DESKTOP_ID, &format.mime])
            .output()
            .map_err(|e| e.to_string())
            .and_then(|r| {
                if r.status.success() {
                    Ok(())
                } else {
                    Err("系统未能更改默认程序".into())
                }
            });
        done(result);
    });
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_actions_preserve_literal_paths_and_multi_open() {
        let text = contents(Path::new("/opt/中文 space/$`100%\"/smartzip-gui")).unwrap();
        assert!(text.contains(" -- %F\n"));
        assert!(text.contains(" --quick-extract -- %F\n"));
        assert!(text.contains("100%%"));
        assert!(text.contains("\\\\$"));
        assert!(text.contains("MimeType=application/zip;"));
        assert!(contents(Path::new("/opt/bad\nExec=sh")).is_err());
    }
}
