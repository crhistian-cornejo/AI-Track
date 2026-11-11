//! Sample-accurate events delivered with each block.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    /// Sample offset inside the block.
    pub offset: u32,
    pub kind: EventKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    NoteOn { channel: u8, key: u8, velocity: f32, note_id: i32 },
    NoteOff { channel: u8, key: u8, velocity: f32, note_id: i32 },
    /// Per-note expression (MPE / CLAP note expressions). `value` range depends on `expr`.
    NoteExpression { note_id: i32, key: u8, expr: Expression, value: f32 },
    /// Normalised parameter change 0..=1.
    Param { id: u32, value: f32 },
    ControlChange { channel: u8, cc: u8, value: f32 },
    /// -1..1.
    PitchBend { channel: u8, value: f32 },
    ChannelPressure { channel: u8, value: f32 },
    AllNotesOff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expression {
    /// Semitones.
    Tuning,
    /// 0..=1.
    Pressure,
    /// -1..=1.
    Pan,
    /// 0..=1 (MPE Y / timbre).
    Brightness,
    /// Linear gain.
    Volume,
}
