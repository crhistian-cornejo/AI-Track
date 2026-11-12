//! Every icon the app draws, embedded in the binary. Icons are 24×24
//! stroke-rounded SVGs painted as monochrome masks, so they take the theme's
//! colours. Some come from Hugeicons (MIT, see `assets/icons/LICENSE`); the
//! DAW-specific ones are drawn for AI-Track in the same style.

use gpui::{Hsla, IntoElement, Styled, Svg, px, svg};

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        /// The icon catalogue. Unused entries stay so views can pick freely.
        #[allow(dead_code)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon { $($variant,)* }

        impl Icon {
            /// Asset path the [`crate::assets::Assets`] source answers to.
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file, ".svg"),)* }
            }
        }

        /// The embedded SVG for an asset path.
        pub fn bytes(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("icons/", $file, ".svg") => Some(include_bytes!(concat!("../assets/icons/", $file, ".svg"))),)*
                _ => None,
            }
        }
    };
}

icons! {
    Add => "add-01",
    Arpeggio => "arpeggio",
    Automation => "automation",
    Bot => "bot",
    Chat => "chatting",
    Check => "check",
    ChevronDown => "chevron-down",
    ChevronLeft => "chevron-left",
    ChevronRight => "chevron-right",
    ChevronUp => "chevron-up",
    Clock => "clock",
    Copy => "copy",
    Cursor => "cursor-magic",
    Dashboard => "dashboard",
    Device => "device",
    Dice => "dice",
    Download => "download",
    Drum => "drum",
    Extract => "extract",
    Eye => "eye",
    File => "file",
    Folder => "folder-01",
    FolderOpen => "folder-open",
    FolderTree => "folder-tree",
    Gauge => "gauge",
    Globe => "globe",
    Grid => "grid",
    Group => "group",
    Headphones => "headphones",
    Help => "help-circle",
    Keyboard => "keyboard",
    Layers => "layers",
    Link => "link",
    List => "list-bullet",
    Loader => "loader",
    Logo => "logo",
    Loop => "loop",
    Magnet => "magnet",
    Metronome => "metronome",
    Mic => "mic-01",
    Minus => "minus-sign",
    Mixer => "mixer",
    Moon => "moon",
    More => "more-horizontal",
    MusicNote => "music-note",
    Package => "package",
    Paste => "paste",
    PanelBottom => "panel-bottom",
    PanelLeft => "panel-left",
    PanelRight => "panel-right",
    Pencil => "pencil-edit",
    Piano => "piano",
    Play => "play",
    Plug => "plug",
    Power => "power",
    Quantize => "quantize",
    Record => "record",
    Redo => "redo",
    Refresh => "refresh-cw",
    Return => "return",
    Sample => "sample",
    ScaleSnap => "scale-snap",
    Scissors => "scissors",
    Search => "search-01",
    Send => "send",
    Shuffle => "shuffle",
    Sliders => "sliders",
    Sparkles => "sparkles",
    Split => "split",
    SquareFill => "square-fill",
    Star => "star",
    Steps => "steps",
    Stop => "stop",
    Strum => "strum",
    Sun => "sun",
    Timeline => "timeline",
    Trash => "trash",
    Undo => "undo-2",
    Volume => "volume",
    Wand => "wand",
    Waveform => "waveform",
    X => "x",
    ZoomIn => "zoom-in",
    ZoomOut => "zoom-out",
}

/// An icon element at `size` px in `color`.
pub fn icon(icon: Icon, size: f32, color: Hsla) -> Svg {
    svg().path(icon.path()).size(px(size)).flex_none().text_color(color)
}

/// Same, as an `IntoElement` for builder chains that want one.
#[allow(dead_code)]
pub fn icon_el(i: Icon, size: f32, color: Hsla) -> impl IntoElement {
    icon(i, size, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_embedded() {
        for i in [Icon::Play, Icon::Record, Icon::Sparkles, Icon::Piano, Icon::Mixer, Icon::ZoomOut] {
            let bytes = bytes(i.path()).expect("embedded");
            assert!(std::str::from_utf8(bytes).unwrap().contains("<svg"));
        }
        assert!(bytes("icons/nope.svg").is_none());
    }
}
