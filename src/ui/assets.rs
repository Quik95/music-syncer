//! The kit's component icons plus the few extra Lucide icons the views draw. `Assets`
//! embeds only the component set, so an icon outside it renders blank unless listed here.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString, assets};

assets::icon_assets!(ExtraIcons, [Disc3]);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
