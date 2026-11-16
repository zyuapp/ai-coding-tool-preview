//! The files the app ships inside its binary: icons and fonts.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

use crate::embedded::FILES;

/// Serves `icons/*.svg` and `fonts/**` from the binary. An app with assets of its own can wrap this
/// and fall through to it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl Assets {
    pub fn get(path: &str) -> Option<&'static [u8]> {
        FILES.iter().find(|(name, _)| *name == path).map(|(_, bytes)| *bytes)
    }
}

impl AssetSource for Assets {
    /// `icons/<name>.svg?stroke=<width>` is the icon drawn with that stroke width, as Lucide's `strokeWidth`.
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((file, width)) = path.split_once("?stroke=") {
            return Ok(Self::get(file).map(|bytes| {
                let svg = String::from_utf8_lossy(bytes).replacen("stroke-width=\"2\"", &format!("stroke-width=\"{width}\""), 1);
                Cow::Owned(svg.into_bytes())
            }));
        }
        Ok(Self::get(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
