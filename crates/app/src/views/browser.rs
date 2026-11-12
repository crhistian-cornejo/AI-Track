//! What is left of the browser sidebar: the Cmd+K palette
//! ([`crate::views::palette`]) replaced it. These helpers stay because the
//! track headers and the assistant use them.

use crate::icons::Icon;
use aitrack_model::{Scale, TrackKind};

/// "Am", "C", "F#m" — the compact key a badge shows.
pub fn short_key(s: Scale) -> String {
    crate::views::palette::short_key(s)
}

pub fn kind_icon(kind: TrackKind) -> Icon {
    match kind {
        TrackKind::Instrument => Icon::Piano,
        TrackKind::Audio => Icon::Waveform,
        TrackKind::Group => Icon::Group,
        TrackKind::Folder => Icon::Folder,
        TrackKind::Return => Icon::Return,
        TrackKind::Vca => Icon::Sliders,
        TrackKind::Main => Icon::Volume,
    }
}
