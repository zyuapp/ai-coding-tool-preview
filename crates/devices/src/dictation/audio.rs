//! Sound as numbers: what the ONNX dictation helper records, reads from a phone's WAV, and shows as a
//! level (dictation-audio.ts).

/// Signed 16-bit little-endian samples as floats from -1 to 1. A trailing odd byte is dropped.
pub fn pcm16_samples(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32_768.0).collect()
}

/// How loud a stretch of samples is, from 0 to 1 in two-decimal steps: -55 dB reads as silence and -5 dB as full.
pub fn audio_level(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f64 = samples.iter().map(|sample| f64::from(*sample) * f64::from(*sample)).sum();
    let rms = (energy / samples.len() as f64).sqrt();
    let level = ((20.0 * rms.max(1e-6).log10() + 55.0) / 50.0).clamp(0.0, 1.0);
    (level * 100.0).round() / 100.0
}

/// Sound at another rate. Going down, each new sample averages the old ones it spans; going up, it
/// reads between its two neighbours.
pub fn resampled(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let length = ((samples.len() as f64 / ratio).round() as usize).max(1);
    (0..length)
        .map(|index| {
            let start = index as f64 * ratio;
            if ratio > 1.0 {
                let first = start.floor() as usize;
                let last = samples.len().min((first + 1).max((start + ratio).floor() as usize));
                let first = first.min(last.saturating_sub(1));
                let sum: f32 = samples[first..last].iter().sum();
                sum / (last - first).max(1) as f32
            } else {
                let before = (samples.len() - 1).min(start.floor() as usize);
                let after = (samples.len() - 1).min(before + 1);
                let weight = (start - before as f64) as f32;
                samples[before] * (1.0 - weight) + samples[after] * weight
            }
        })
        .collect()
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum WavError {
    #[error("Not a WAV file.")]
    NotWav,
    #[error("The WAV file has no sound.")]
    NoSound,
    #[error("The WAV file's format can't be read.")]
    Format,
}

const PCM: u16 = 1;
const FLOAT: u16 = 3;
const EXTENSIBLE: u16 = 0xfffe;

/// A WAV file's sound, mixed to mono at its own rate. Reads 16-bit PCM and 32-bit float.
pub fn wav_samples(bytes: &[u8]) -> Result<(Vec<f32>, u32), WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    let u16_at = |data: &[u8], at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
    let u32_at = |data: &[u8], at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(bytes, at + 4) as usize;
        let body = &bytes[at + 8..bytes.len().min(at + 8 + size)];
        if id == b"fmt " && body.len() >= 16 {
            let tag = u16_at(body, 0);
            let encoding = if tag == EXTENSIBLE && body.len() >= 26 { u16_at(body, 24) } else { tag };
            format = Some((encoding, u16_at(body, 2), u32_at(body, 4), u16_at(body, 14)));
        } else if id == b"data" {
            data = Some(body);
        }
        at += 8 + size + (size % 2);
    }
    let (Some((encoding, channels, rate, bits)), Some(data)) = (format, data) else { return Err(WavError::NoSound) };
    let pcm = encoding == PCM && bits == 16;
    if !(pcm || (encoding == FLOAT && bits == 32)) || channels < 1 || rate < 1 {
        return Err(WavError::Format);
    }
    let width = usize::from(bits / 8);
    let channels = usize::from(channels);
    let frames = data.len() / (width * channels);
    let samples = (0..frames)
        .map(|frame| {
            let sum: f32 = (0..channels)
                .map(|channel| {
                    let at = (frame * channels + channel) * width;
                    if pcm {
                        i16::from_le_bytes([data[at], data[at + 1]]) as f32 / 32_768.0
                    } else {
                        f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
                    }
                })
                .sum();
            sum / channels as f32
        })
        .collect();
    Ok((samples, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let mut out = b"RIFF".to_vec();
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(channels.to_le_bytes());
        out.extend(rate.to_le_bytes());
        out.extend((rate * u32::from(channels) * 2).to_le_bytes());
        out.extend((channels * 2).to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn stereo_wav_mixes_to_mono() {
        let (samples, rate) = wav_samples(&wav(2, 8_000, &[16_384, 0, -16_384, -16_384])).unwrap();
        assert_eq!(rate, 8_000);
        assert_eq!(samples, [0.25, -0.5]);
        assert_eq!(wav_samples(b"RIFF0000WAVX"), Err(WavError::NotWav));
    }

    #[test]
    fn levels_run_from_silence_to_full() {
        assert_eq!(audio_level(&[]), 0.0);
        assert_eq!(audio_level(&[0.0; 10]), 0.0);
        assert_eq!(audio_level(&[1.0, -1.0]), 1.0);
    }

    #[test]
    fn resampling_keeps_length_proportional() {
        let samples: Vec<f32> = (0..48_000).map(|index| (index % 7) as f32).collect();
        assert_eq!(resampled(&samples, 48_000, 16_000).len(), 16_000);
        assert_eq!(resampled(&[0.0, 1.0], 1, 2), [0.0, 0.5, 1.0, 1.0]);
    }
}
