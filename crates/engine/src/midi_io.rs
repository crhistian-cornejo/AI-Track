//! Live MIDI input via midir.
//!
//! Each open port gets its own wait-free SPSC queue straight into the audio
//! thread: the midir callback thread pushes raw messages and the audio
//! callback turns them into events for the MIDI target track (or every
//! armed instrument track). The UI thread is never on this path.

use crate::rt::TimedMidi;
use crate::EngineError;
use rtrb::Producer;
use std::time::Instant;

/// A MIDI input port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortInfo {
    /// Stable id to pass to `EngineHandle::open_midi_input`.
    pub id: String,
    pub name: String,
}

/// A MIDI input port and whether the engine has it open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortStatus {
    /// Stable id to pass to `EngineHandle::open_midi_input`.
    pub id: String,
    pub name: String,
    /// Messages from this port reach the engine.
    pub open: bool,
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

/// What the midir callback owns: the queue into the audio thread and the
/// engine clock its messages are stamped with.
pub(crate) struct PortSink {
    tx: Producer<TimedMidi>,
    epoch: Instant,
}

/// An open input; dropping it closes the port.
pub(crate) struct MidiConnection {
    pub id: String,
    conn: midir::MidiInputConnection<PortSink>,
}

impl MidiConnection {
    /// Close the port and take its queue back, so toggling a port on and off
    /// never uses up the fixed set of queues.
    pub fn close(self) -> Producer<TimedMidi> {
        self.conn.close().1.tx
    }
}

/// Open `id`, taking one queue from `spare` once the port is known to exist
/// (a missing port costs none). Each message is stamped, when the driver delivers it, with the
/// time on the engine clock (`epoch` = `SharedStatus::epoch`): the audio
/// thread turns the stamp into a sample offset inside its block. (midir's own
/// stamp is relative to an unspecified start and some backends leave it 0.)
pub(crate) fn connect(id: &str, spare: &mut Vec<Producer<TimedMidi>>, epoch: Instant) -> Result<MidiConnection, EngineError> {
    let mut input = midir::MidiInput::new("AI-Track").map_err(|e| EngineError::Midi(e.to_string()))?;
    input.ignore(midir::Ignore::All);
    let port = input.find_port_by_id(id).ok_or_else(|| EngineError::Midi(format!("MIDI port '{id}' not found")))?;
    let producer = spare.pop().ok_or_else(|| EngineError::Midi("too many MIDI inputs open".into()))?;
    let conn = input
        .connect(
            &port,
            "aitrack-in",
            |_stamp, data, sink: &mut PortSink| {
                if let Some(msg) = crate::rt::MidiMessage::from_slice(data) {
                    let nanos = (sink.epoch.elapsed().as_nanos() as u64).max(1);
                    let _ = sink.tx.push(TimedMidi { msg, nanos });
                }
            },
            PortSink { tx: producer, epoch },
        )
        .map_err(|e| EngineError::Midi(e.to_string()))?;
    Ok(MidiConnection { id: id.to_string(), conn })
}
