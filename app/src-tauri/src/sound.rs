//! Alert sounds, synthesised at runtime (no audio assets to ship).
//!
//! Each alert kind plays a tone the user picks; the defaults are:
//! - needs input: `Bell`, two quick rising strikes played twice — urgent.
//! - done: `Chime`, a short rising major arpeggio — finished, no rush.
//! - usage level: `Soft`, two soft falling notes.

use std::num::NonZero;
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

use rodio::buffer::SamplesBuffer;
use rodio::Sample;
use serde::{Deserialize, Serialize};

const RATE: u32 = 44_100;

/// What happened (each kind has its own on/off switch and tone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    NeedsInput,
    Done,
    Threshold,
}

/// How it sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Bell,
    Chime,
    Soft,
    Ping,
    Pulse,
}

#[cfg(test)]
const TONES: [Tone; 5] = [Tone::Bell, Tone::Chime, Tone::Soft, Tone::Ping, Tone::Pulse];

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Tones {
    #[serde(default = "bell")]
    pub needs_input: Tone,
    #[serde(default = "chime")]
    pub done: Tone,
    #[serde(default = "soft")]
    pub threshold: Tone,
}

fn bell() -> Tone {
    Tone::Bell
}
fn chime() -> Tone {
    Tone::Chime
}
fn soft() -> Tone {
    Tone::Soft
}

impl Default for Tones {
    fn default() -> Self {
        Self {
            needs_input: bell(),
            done: chime(),
            threshold: soft(),
        }
    }
}

impl Tones {
    pub fn get(&self, sound: Sound) -> Tone {
        match sound {
            Sound::NeedsInput => self.needs_input,
            Sound::Done => self.done,
            Sound::Threshold => self.threshold,
        }
    }

    pub fn set(&mut self, sound: Sound, tone: Tone) {
        match sound {
            Sound::NeedsInput => self.needs_input = tone,
            Sound::Done => self.done = tone,
            Sound::Threshold => self.threshold = tone,
        }
    }
}

/// Plays without blocking at `volume` (0–100). Audio failures (no device,
/// busy) are ignored: the desktop notification is still shown.
pub fn play(tone: Tone, volume: u8) {
    static PLAYER: OnceLock<Sender<(Tone, u8)>> = OnceLock::new();
    let tx = PLAYER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(Tone, u8)>();
        std::thread::spawn(move || {
            // Opened once and kept: reopening per alert adds latency and clicks.
            let mut sink = match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(sink) => sink,
                Err(e) => {
                    eprintln!("ai-usage: no audio output, alert sounds disabled: {e}");
                    return;
                }
            };
            sink.log_on_drop(false);
            for (tone, volume) in rx {
                sink.mixer().add(SamplesBuffer::new(
                    NonZero::new(1).unwrap(),
                    NonZero::new(RATE).unwrap(),
                    render(tone, volume),
                ));
            }
        });
        tx
    });
    if volume > 0 {
        let _ = tx.send((tone, volume.min(100)));
    }
}

/// (frequency Hz, start s, length s)
fn notes(tone: Tone) -> &'static [(f32, f32, f32)] {
    match tone {
        Tone::Bell => &[
            (1318.5, 0.00, 0.35), // E6
            (1760.0, 0.14, 0.45), // A6
            (1318.5, 0.55, 0.35),
            (1760.0, 0.69, 0.60),
        ],
        Tone::Chime => &[
            (523.25, 0.00, 0.40),  // C5
            (659.25, 0.11, 0.40),  // E5
            (783.99, 0.22, 0.40),  // G5
            (1046.50, 0.33, 0.90), // C6
        ],
        Tone::Soft => &[
            (880.0, 0.00, 0.40),  // A5
            (659.25, 0.18, 0.70), // E5
        ],
        Tone::Ping => &[
            (1567.98, 0.00, 0.55), // G6, a single short strike
        ],
        Tone::Pulse => &[
            (987.77, 0.00, 0.12), // B5 ×4, quick and even
            (987.77, 0.18, 0.12),
            (987.77, 0.36, 0.12),
            (987.77, 0.54, 0.30),
        ],
    }
}

fn render(tone: Tone, volume: u8) -> Vec<Sample> {
    let notes = notes(tone);
    let total = notes.iter().map(|(_, s, l)| s + l).fold(0.0, f32::max);
    let mut buf = vec![0.0f32; (total * RATE as f32) as usize + 1];
    for &(freq, start, len) in notes {
        let offset = (start * RATE as f32) as usize;
        for i in 0..(len * RATE as f32) as usize {
            let t = i as f32 / RATE as f32;
            // 5 ms attack to avoid a click, exponential bell-like decay.
            let env = (t / 0.005).min(1.0) * (-t * 6.0 / len).exp();
            let phase = std::f32::consts::TAU * freq * t;
            // Fundamental plus a soft octave partial for a bell timbre.
            let v = phase.sin() + 0.3 * (2.0 * phase).sin() * (-t * 10.0).exp();
            buf[offset + i] += v * env;
        }
    }
    let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    // 100% is a peak of 0.35, loud enough over music without clipping;
    // squared so the slider feels even to the ear.
    let gain = 0.35 * (f32::from(volume) / 100.0).powi(2);
    buf.into_iter()
        .map(|v| (v / peak * gain) as Sample)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_sounds_are_short_and_not_clipping() {
        for tone in TONES {
            let samples = render(tone, 100);
            let secs = samples.len() as f32 / RATE as f32;
            assert!((0.5..2.0).contains(&secs), "{tone:?} lasts {secs}s");
            assert!(samples.iter().all(|s| s.abs() <= 0.36));
        }
    }
}
