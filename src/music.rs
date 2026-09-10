use rodio::{DeviceSinkBuilder, MixerDeviceSink, Source, buffer::SamplesBuffer};

use std::time::{Duration, Instant};

const SAMPLE_RATE: u32 = 22_050;
const STEP_SECONDS: f32 = 60.0 / 111.0 / 2.0;

/// The device owns playback. Dropping it stops music, including on exit.
pub(crate) struct Music {
    _device: MixerDeviceSink,
    started: Instant,
    levels: Vec<char>,
    duration: Duration,
}

impl Music {
    pub(crate) fn start() -> anyhow::Result<Self> {
        let mut device = DeviceSinkBuilder::open_default_sink()?;
        device.log_on_drop(false);
        let samples = render_loop();
        let levels = volume_levels(&samples);
        let duration = Duration::from_secs_f64(samples.len() as f64 / SAMPLE_RATE as f64);
        let source = SamplesBuffer::new(
            1.try_into().unwrap(),
            SAMPLE_RATE.try_into().unwrap(),
            samples,
        )
        .repeat_infinite();
        device.mixer().add(source);
        Ok(Self {
            _device: device,
            started: Instant::now(),
            levels,
            duration,
        })
    }

    pub(crate) fn animation(&self) -> String {
        volume_trail(&self.levels, self.started.elapsed(), self.duration)
    }
}

/// One volume level per 100 ms of synthesized audio. This is a waveform trail,
/// not a frequency spectrum. Precompute it so drawing needs no audio analysis.
fn volume_levels(samples: &[f32]) -> Vec<char> {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    samples
        .chunks(SAMPLE_RATE as usize / 10)
        .map(|chunk| {
            let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
            BARS[((rms * 80.0) as usize).min(BARS.len() - 1)]
        })
        .collect()
}

fn volume_trail(levels: &[char], elapsed: Duration, duration: Duration) -> String {
    let frame = ((elapsed.as_secs_f64() % duration.as_secs_f64()) * SAMPLE_RATE as f64
        / (SAMPLE_RATE / 10) as f64) as usize;
    (0..12)
        .map(|column| {
            let index = (frame + levels.len() - (11 - column) % levels.len()) % levels.len();
            levels[index]
        })
        .collect()
}

/// Four-bar piano phrase, quantized to sixteenth notes at 111 BPM.
/// Reference: https://bitmidi.com/tupac-shakur-changes-mid (piano, beats 40–56).
/// Each event is (start beat, duration in beats, MIDI pitches). The chord voices
/// overlap the passing notes; a single monophonic line loses the recognizable hook.
fn render_loop() -> Vec<f32> {
    let piano: &[(f32, f32, &[u8])] = &[
        (0.0, 1.8, &[76, 81, 84]),
        (2.0, 1.75, &[76, 79, 83]),
        (4.0, 1.35, &[74, 78]),
        (4.0, 0.7, &[81]),
        (4.75, 0.6, &[79]),
        (5.5, 1.3, &[74]),
        (5.5, 2.05, &[72, 79]),
        (7.0, 0.7, &[67]),
        (8.0, 1.8, &[67, 74]),
        (8.0, 0.85, &[71]),
        (8.75, 0.7, &[69]),
        (9.5, 0.25, &[71]),
        (10.0, 1.8, &[69, 78]),
        (10.0, 0.8, &[74]),
        (10.75, 0.8, &[76]),
        (11.5, 0.35, &[74]),
        (12.0, 1.3, &[72]),
        (12.0, 1.45, &[74]),
        (12.0, 3.3, &[67]),
        (13.5, 1.9, &[72]),
        (15.0, 0.4, &[67]),
        (15.5, 0.24, &[81]),
        (15.75, 0.24, &[83]),
    ];
    let bass: &[(f32, f32, &[u8])] = &[
        (0.0, 1.8, &[45]),
        (2.0, 1.8, &[40]),
        (4.0, 1.35, &[38]),
        (5.5, 2.25, &[36]),
        (8.0, 1.8, &[31]),
        (10.0, 1.8, &[38]),
        (12.0, 3.8, &[36]),
    ];
    let beat_seconds = STEP_SECONDS * 2.0;
    let mut samples = vec![0.0; (SAMPLE_RATE as f32 * beat_seconds * 16.0) as usize];
    for (events, volume) in [(piano, 0.085), (bass, 0.10)] {
        for &(beat, duration, notes) in events {
            let start = (beat * beat_seconds * SAMPLE_RATE as f32) as usize;
            let seconds = duration * beat_seconds;
            let length = (seconds * SAMPLE_RATE as f32) as usize;
            for (i, sample) in samples.iter_mut().skip(start).take(length).enumerate() {
                let t = i as f32 / SAMPLE_RATE as f32;
                let envelope = (t / 0.003).min(1.0)
                    * ((seconds - t) / 0.015).clamp(0.0, 1.0)
                    * (-t * 2.5).exp();
                for &note in notes {
                    *sample += pulse(note, t) * envelope * volume;
                }
            }
        }
    }
    let mut noise = 1_u32;
    for (i, sample) in samples.iter_mut().enumerate() {
        let time = i as f32 / SAMPLE_RATE as f32;
        let beat = (time / beat_seconds) as usize;
        let t = time % beat_seconds;
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        let hiss = noise as f32 / u32::MAX as f32 * 2.0 - 1.0;
        let hat = hiss * (-(time % STEP_SECONDS) * 90.0).exp() * 0.025;
        let drum = if beat.is_multiple_of(2) {
            (std::f32::consts::TAU * (48.0 * t + 2.0 * (1.0 - (-t * 30.0).exp()))).sin()
                * (-t * 22.0).exp()
                * 0.10
        } else {
            hiss * (-t * 25.0).exp() * 0.065
        };
        // Eight-bit amplitude steps retain the handheld-console sound.
        *sample = ((*sample + hat + drum) * 128.0).round() / 128.0;
    }
    samples
}

fn pulse(note: u8, time: f32) -> f32 {
    let frequency = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
    if (frequency * time).fract() < 0.25 {
        1.0
    } else {
        -1.0 / 3.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_tracks_volume_and_wraps_at_the_loop_end() {
        let mut samples = vec![0.0; SAMPLE_RATE as usize / 10];
        samples.extend(vec![0.1; SAMPLE_RATE as usize / 10]);
        let levels = volume_levels(&samples);
        assert_eq!(levels, vec!['▁', '█']);
        let duration = Duration::from_millis(200);
        let first = volume_trail(&levels, Duration::ZERO, duration);
        let next = volume_trail(&levels, Duration::from_millis(100), duration);
        assert_eq!(first.chars().count(), 12);
        assert!(first.ends_with('▁'));
        assert!(next.ends_with('█'));
        assert_ne!(first, next);
        assert_eq!(first, volume_trail(&levels, duration, duration));
    }

    #[test]
    fn opening_plays_the_reference_a_minor_chord() {
        let samples = render_loop();
        // The reference opens with E5/A5/C6 together, held beyond one eighth note.
        // Check rendered audio, not just the score, so lost voices or timing fail.
        for start in [0.05, 0.35] {
            let power = |note: u8| {
                let frequency = 440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0);
                let mut real = 0.0;
                let mut imaginary = 0.0;
                for (i, sample) in samples
                    .iter()
                    .skip((start * SAMPLE_RATE as f32) as usize)
                    .take(2205)
                    .enumerate()
                {
                    let phase = std::f32::consts::TAU * frequency * i as f32 / SAMPLE_RATE as f32;
                    real += sample * phase.cos();
                    imaginary += sample * phase.sin();
                }
                real * real + imaginary * imaginary
            };
            for note in [76, 81, 84] {
                assert!(
                    power(note) > power(79) * 4.0,
                    "missing opening chord note {note} at {start}s"
                );
            }
        }
    }

    #[test]
    fn loop_has_sound_headroom_and_complete_steps() {
        let samples = render_loop();
        assert_eq!(
            samples.len(),
            (SAMPLE_RATE as f32 * STEP_SECONDS * 32.0) as usize
        );
        assert!(samples.iter().all(|s| s.is_finite() && s.abs() < 0.5));
        assert!(samples.iter().any(|s| s.abs() > 0.1));
        assert_eq!(samples, render_loop());
    }
}
