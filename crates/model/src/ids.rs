//! Stable identifiers. Ids are allocated by the [`Project`](crate::Project)
//! and never reused, so undo/redo and agent proposals can refer to objects
//! across edits.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}#{}", stringify!($name), self.0)
            }
        }
    )*};
}

id_type!(TrackId, ClipId, SourceId, NoteId, DeviceId, SceneId, PlacementId);

/// Monotonic id allocator stored in the project file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IdAllocator {
    next: u64,
}

impl IdAllocator {
    pub fn next_raw(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// Make sure future ids are above `seen` (used after loading or applying
    /// commands that carry ids created elsewhere, such as redo).
    pub fn observe(&mut self, seen: u64) {
        self.next = self.next.max(seen);
    }
}
