//! Embedded assets: icons answer by path, fonts load at start-up.

use gpui::{App, AssetSource, Result, SharedString};
use std::borrow::Cow;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(crate::icons::bytes(path).map(Cow::Borrowed))
    }

    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// Geist and Geist Mono (SIL OFL 1.1, see `assets/fonts/Geist.LICENSE`).
const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
];

pub fn load_fonts(cx: &App) {
    let fonts = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("AI-Track: cannot load the bundled fonts: {e}");
    }
}
