//! Alert sounds, synthesised at runtime (no audio assets to ship).
//!
//! - `NeedsInput`: two quick rising bell strikes, played twice — urgent.
//! - `Done`: a short rising major arpeggio — finished, no rush.
//! - `Threshold`: two soft falling notes — usage crossed an alert level.

use std::num::NonZero;
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

use rodio::buffer::SamplesBuffer;
use rodio::Sample;
use serde::Deserialize;

const RATE: u32 = 44_100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    NeedsInput,
    Done,
    Threshold,
}

/// Plays without blocking. Audio failures (no device, busy) are ignored:
/// the desktop notification is still shown.
pub fn play(sound: Sound) {
    static PLAYER: OnceLock<Sender<Sound>> = OnceLock::new();
    let tx = PLAYER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Sound>();
        std::thread::spawn(move || {
            // Opened once and kept: reopening per alert adds latency and clicks.
            let mut sink = match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(sink) => sink,
                Err(e) => {
                    eprintln!("ia-usage: no audio output, alert sounds disabled: {e}");
                    return;
                }
            };
            sink.log_on_drop(false);
            for sound in rx {
                sink.mixer().add(SamplesBuffer::new(
                    NonZero::new(1).unwrap(),
                    NonZero::new(RATE).unwrap(),
                    render(sound),
                ));
            }
        });
        tx
    });
    let _ = tx.send(sound);
}

/// (frequency Hz, start s, length s)
fn notes(sound: Sound) -> &'static [(f32, f32, f32)] {
    match sound {
        Sound::NeedsInput => &[
            (1318.5, 0.00, 0.35), // E6
            (1760.0, 0.14, 0.45), // A6
            (1318.5, 0.55, 0.35),
            (1760.0, 0.69, 0.60),
        ],
        Sound::Done => &[
            (523.25, 0.00, 0.40),  // C5
            (659.25, 0.11, 0.40),  // E5
            (783.99, 0.22, 0.40),  // G5
            (1046.50, 0.33, 0.90), // C6
        ],
        Sound::Threshold => &[
            (880.0, 0.00, 0.40),  // A5
            (659.25, 0.18, 0.70), // E5
        ],
    }
}

fn render(sound: Sound) -> Vec<Sample> {
    let notes = notes(sound);
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
    buf.into_iter()
        .map(|v| (v / peak * 0.35) as Sample)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_sounds_are_short_and_not_clipping() {
        for sound in [Sound::NeedsInput, Sound::Done, Sound::Threshold] {
            let samples = render(sound);
            let secs = samples.len() as f32 / RATE as f32;
            assert!((0.5..2.0).contains(&secs), "{sound:?} lasts {secs}s");
            assert!(samples.iter().all(|s| s.abs() <= 0.36));
        }
    }
}
