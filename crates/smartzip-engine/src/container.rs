use smartzip_core::BusinessContainerKind;
use std::fs::File;
use std::path::Path;
use zip::ZipArchive;

/// Check if a ZIP listing indicates a business container.
///
/// Only inspects entry paths — does not extract or decompress.
pub fn classify_zip_listing(
    entry_paths: &[String],
    has_archive_entries: bool,
) -> Option<BusinessContainerKind> {
    let has = |name: &str| -> bool {
        entry_paths
            .iter()
            .any(|e| e == name || e.ends_with(&format!("/{name}")))
    };

    if has("[Content_Types].xml") {
        if has("word/document.xml") {
            return Some(BusinessContainerKind::OfficeDocx);
        }
        if has("xl/workbook.xml") {
            return Some(BusinessContainerKind::OfficeXlsx);
        }
        if has("ppt/presentation.xml") {
            return Some(BusinessContainerKind::OfficePptx);
        }
    }

    if has("mimetype") && has("META-INF/container.xml") {
        return Some(BusinessContainerKind::Epub);
    }

    if has("AndroidManifest.xml") && (has("classes.dex") || has("resources.arsc")) {
        return Some(BusinessContainerKind::Apk);
    }

    if has("META-INF/MANIFEST.MF") && has_archive_entries {
        let has_class = entry_paths.iter().any(|e| e.ends_with(".class"));
        if has_class {
            return Some(BusinessContainerKind::Jar);
        }
    }

    if !entry_paths.is_empty() && !has_archive_entries {
        let image_exts = [".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp"];
        let image_count = entry_paths
            .iter()
            .filter(|e| {
                let lower = e.to_ascii_lowercase();
                image_exts.iter().any(|ext| lower.ends_with(ext))
            })
            .count();
        if image_count * 3 >= entry_paths.len() * 2 {
            return Some(BusinessContainerKind::Cbz);
        }
    }

    None
}

pub fn classify_zip_path(path: &Path) -> Option<BusinessContainerKind> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;
    let mut entry_paths = Vec::new();
    let mut has_archive_entries = false;

    for index in 0..archive.len() {
        let entry = archive.by_index(index).ok()?;
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        has_archive_entries |= matches!(
            Path::new(&lower)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz")
        );
        entry_paths.push(name);
    }

    classify_zip_listing(&entry_paths, has_archive_entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_docx() {
        let entries = vec![
            "[Content_Types].xml".into(),
            "word/document.xml".into(),
            "word/styles.xml".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::OfficeDocx)
        );
    }

    #[test]
    fn detects_xlsx() {
        let entries = vec![
            "[Content_Types].xml".into(),
            "xl/workbook.xml".into(),
            "xl/sharedStrings.xml".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::OfficeXlsx)
        );
    }

    #[test]
    fn detects_epub() {
        let entries = vec![
            "mimetype".into(),
            "META-INF/container.xml".into(),
            "OEBPS/content.opf".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::Epub)
        );
    }

    #[test]
    fn detects_apk() {
        let entries = vec![
            "AndroidManifest.xml".into(),
            "classes.dex".into(),
            "resources.arsc".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::Apk)
        );
    }

    #[test]
    fn detects_cbz() {
        let entries = vec![
            "001.jpg".into(),
            "002.jpg".into(),
            "003.jpg".into(),
            "004.png".into(),
        ];
        assert_eq!(
            classify_zip_listing(&entries, false),
            Some(BusinessContainerKind::Cbz)
        );
    }

    #[test]
    fn fake_docx_real_zip_not_detected() {
        let entries = vec!["file1.txt".into(), "file2.txt".into()];
        assert_eq!(classify_zip_listing(&entries, false), None);
    }

    #[test]
    fn plain_zip_not_detected() {
        let entries = vec!["data.bin".into(), "readme.txt".into()];
        assert_eq!(classify_zip_listing(&entries, false), None);
    }
}
