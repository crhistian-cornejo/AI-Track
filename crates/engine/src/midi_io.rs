//! Live MIDI input via midir.
//!
//! Each open port gets its own wait-free SPSC queue straight into the audio
//! thread: the midir callback thread pushes raw messages and the audio
//! callback turns them into events for the MIDI target track (or every
//! armed instrument track). The UI thread is never on this path.

use crate::rt::MidiMessage;
use crate::EngineError;
use rtrb::Producer;

/// A MIDI input port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortInfo {
    /// Stable id to pass to `EngineHandle::open_midi_input`.
    pub id: String,
    pub name: String,
}

/// List MIDI input ports.
pub fn midi_inputs() -> Vec<MidiPortInfo> {
    let Ok(input) = midir::MidiInput::new("AI-Track") else { return Vec::new() };
    input
        .ports()
        .iter()
        .map(|p| MidiPortInfo { id: p.id(), name: input.port_name(p).unwrap_or_else(|_| p.id()) })
        .collect()
}

/// An open input; dropping it closes the port.
pub(crate) struct MidiConnection {
    pub id: String,
    _conn: midir::MidiInputConnection<Producer<MidiMessage>>,
}

pub(crate) fn connect(id: &str, producer: Producer<MidiMessage>) -> Result<MidiConnection, EngineError> {
    let mut input = midir::MidiInput::new("AI-Track").map_err(|e| EngineError::Midi(e.to_string()))?;
    input.ignore(midir::Ignore::All);
    let port = input.find_port_by_id(id).ok_or_else(|| EngineError::Midi(format!("MIDI port '{id}' not found")))?;
    let conn = input
        .connect(
            &port,
            "aitrack-in",
            |_stamp, data, tx: &mut Producer<MidiMessage>| {
                if let Some(m) = MidiMessage::from_slice(data) {
                    let _ = tx.push(m);
                }
            },
            producer,
        )
        .map_err(|e| EngineError::Midi(e.to_string()))?;
    Ok(MidiConnection { id: id.to_string(), _conn: conn })
}
