use gpui::AssetSource;

gpui::assets::icon_assets!(
    SmartZipIcons,
    [
        AppWindow,
        ChevronLeft,
        Clock,
        Cpu,
        Ellipsis,
        X,
        FileArchive,
        FolderOpen,
        FolderSearch,
        KeyRound,
        Layers,
        PackageOpen,
        PanelLeftOpen,
        Plus,
        ScanSearch,
        ShieldCheck,
        SlidersHorizontal,
    ]
);

/// Keeps gpui-kit's component assets while adding only SmartZip's icons.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = SmartZipIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        let mut paths = gpui::assets::Assets.list(path)?;
        paths.extend(SmartZipIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::Assets;
    use gpui::assets::IconName;
    use gpui::AssetSource;

    #[test]
    fn every_smartzip_icon_is_embedded() {
        let assets = Assets;
        for icon in [
            IconName::AppWindow,
            IconName::ChevronLeft,
            IconName::Clock,
            IconName::Cpu,
            IconName::Ellipsis,
            IconName::X,
            IconName::FileArchive,
            IconName::ChevronDown,
            IconName::ChevronRight,
            IconName::FolderOpen,
            IconName::FolderSearch,
            IconName::KeyRound,
            IconName::Layers,
            IconName::PackageOpen,
            IconName::PanelLeftOpen,
            IconName::Plus,
            IconName::ScanSearch,
            IconName::ShieldCheck,
            IconName::SlidersHorizontal,
        ] {
            assert!(
                assets.load(&icon.path()).unwrap().is_some(),
                "{}",
                icon.path()
            );
        }
    }
}

/// A static regular face avoids platform variable-font weight differences.
pub(crate) fn load_fonts(cx: &mut gpui::App) {
    if let Err(error) = cx.text_system().add_fonts(vec![std::borrow::Cow::Borrowed(
        include_bytes!("../../../resources/fonts/NotoSansCJKsc-Regular.otf").as_slice(),
    )]) {
        eprintln!("无法加载界面字体：{error}");
    }
}
