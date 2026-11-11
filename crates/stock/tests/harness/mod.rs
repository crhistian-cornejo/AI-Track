//! Shared driver for the registry-wide integration tests.
#![allow(dead_code)]

use aitrack_dsp::{AudioBuffer, Category, Event, EventKind, ProcessContext, Processor, Transport};

pub const SR: f64 = 48_000.0;
pub const BLOCK: usize = 128;

/// Pre-allocated driver: owns buffers so the measured loop allocates nothing.
pub struct Driver {
    pub buf: AudioBuffer,
    pub sc: AudioBuffer,
    pub events: Vec<Event>,
    pub transport: Transport,
    pub noise: u32,
    pub frame: u64,
}

impl Driver {
    pub fn new() -> Self {
        Self {
            buf: AudioBuffer::new(2, BLOCK),
            sc: AudioBuffer::new(2, BLOCK),
            events: Vec::with_capacity(64),
            transport: Transport { playing: true, bpm: 128.0, ..Transport::default() },
            noise: 12345,
            frame: 0,
        }
    }

    fn rand(&mut self) -> f32 {
        let mut x = self.noise;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise = x;
        (x >> 8) as f32 / 8_388_608.0 - 1.0
    }

    /// Fill the input with a signal: 0 silence, 1 impulse, 2 noise, 3 DC,
    /// 4 loud noise (+40 dB), 5 sine sweep-ish.
    pub fn fill(&mut self, kind: usize) {
        let frames = self.buf.frames();
        for ch in 0..2 {
            for i in 0..frames {
                let n = self.frame + i as u64;
                let v = match kind {
                    0 => 0.0,
                    1 => {
                        if n.is_multiple_of(4096) {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    2 => self.rand() * 0.5,
                    3 => 1.0,
                    4 => self.rand() * 100.0,
                    _ => (n as f32 * 0.02 * (1.0 + (n as f32 * 1e-5).sin())).sin() * 0.8,
                };
                self.buf.channel_mut(ch)[i] = v;
                self.sc.channel_mut(ch)[i] = v * 0.5;
            }
        }
    }

    /// Run one block. Instruments get notes every so often.
    pub fn block(&mut self, p: &mut dyn Processor, signal: usize) {
        let inst = p.info().category == Category::Instrument;
        self.events.clear();
        if inst {
            self.buf.clear();
            let k = self.frame / BLOCK as u64;
            if k.is_multiple_of(8) {
                for (j, key) in [36u8, 48, 55, 60, 64, 67, 71, 74].iter().enumerate() {
                    let key = key + ((k / 8) % 5) as u8;
                    self.events.push(Event { offset: j as u32 * 3, kind: EventKind::NoteOn { channel: 0, key, velocity: 0.8, note_id: key as i32 } });
                }
            } else if k % 8 == 5 {
                for key in 30u8..90 {
                    self.events.push(Event { offset: 10, kind: EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 } });
                    if self.events.len() >= 60 {
                        break;
                    }
                }
            }
            if k % 16 == 3 {
                self.events.push(Event { offset: 20, kind: EventKind::PitchBend { channel: 0, value: 0.5 } });
                self.events.push(Event { offset: 21, kind: EventKind::ControlChange { channel: 0, cc: 1, value: 0.7 } });
            }
        } else {
            self.fill(signal);
        }
        // A parameter change event mid-block.
        if let Some(info) = p.info().params.get((self.frame / BLOCK as u64) as usize % p.info().params.len().max(1)) {
            let v = p.get_param(info.id);
            self.events.push(Event { offset: (BLOCK / 2) as u32, kind: EventKind::Param { id: info.id, value: v } });
        }
        self.events.sort_by_key(|e| e.offset);
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut self.buf, sidechain: Some(&self.sc), events: &self.events, transport: &self.transport };
        p.process(&mut ctx);
        self.frame += BLOCK as u64;
        self.transport.position_samples += BLOCK as i64;
        self.transport.position_beats += BLOCK as f64 / SR * self.transport.bpm / 60.0;
    }

    pub fn output_ok(&self, limit: f32) -> Result<(), String> {
        for ch in 0..2 {
            for (i, &v) in self.buf.channel(ch).iter().enumerate() {
                if !v.is_finite() || v.abs() > limit {
                    return Err(format!("ch{ch}[{i}] = {v}"));
                }
            }
        }
        Ok(())
    }
}
