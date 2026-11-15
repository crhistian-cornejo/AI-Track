//! MIDI effects (`Category::MidiEffect`): devices that turn the notes of
//! their chain into new ones before the instrument sees them. They are
//! driven through `Processor::process_midi`; the audio passes untouched.

pub mod echo;
pub mod kit;
pub mod length;
pub mod pitch;
pub mod random;
pub mod arp;
#[cfg(test)]
mod fxtest;
pub mod theory;
pub mod chord;
pub mod scale;
pub mod velocity;
