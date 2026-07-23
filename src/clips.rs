use std::{
    ffi::OsStr,
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use symphonia::core::{
    audio::sample::Sample,
    codecs::audio::AudioDecoderOptions,
    errors::Error as SymphoniaError,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
};

const SUPPORTED_EXTENSIONS: &[&str] = &["aif", "aiff", "caf", "flac", "m4a", "mp3", "ogg", "wav"];

#[derive(Clone, Debug)]
pub struct DecodedClip {
    pub name: Arc<str>,
    pub samples: Arc<[f32]>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl DecodedClip {
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.channels) / f64::from(self.sample_rate)
    }
}

#[must_use]
pub fn discover(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = directory.read_dir() else {
        return Vec::new();
    };

    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported(path))
        .collect();
    paths.sort_by_cached_key(|path| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    });
    paths
}

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

pub fn decode(path: &Path) -> anyhow::Result<DecodedClip> {
    let file = Box::new(File::open(path)?);
    let media = MediaSourceStream::new(file, MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(OsStr::to_str) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        media,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow::anyhow!("{} has no audio track", path.display()))?;
    let codec_parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or_else(|| anyhow::anyhow!("{} has no audio codec parameters", path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(codec_parameters, &AudioDecoderOptions::default())?;
    let track_id = track.id;

    let mut samples = Vec::new();
    let mut sample_rate = None;
    let mut channels = None;
    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                let spec = audio.spec();
                sample_rate = Some(spec.rate());
                channels = Some(
                    u16::try_from(spec.channels().count())
                        .map_err(|_| anyhow::anyhow!("clip has too many channels"))?,
                );
                let start = samples.len();
                samples.resize(start + audio.samples_interleaved(), f32::MID);
                audio.copy_to_slice_interleaved(&mut samples[start..]);
            }
            Err(SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }

    if samples.is_empty() {
        anyhow::bail!("{} did not contain decodable audio", path.display());
    }

    Ok(DecodedClip {
        name: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
            .into(),
        samples: samples.into(),
        sample_rate: sample_rate.ok_or_else(|| anyhow::anyhow!("clip sample rate is missing"))?,
        channels: channels.ok_or_else(|| anyhow::anyhow!("clip channel count is missing"))?,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn discovery_is_case_insensitive_sorted_and_filters_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("z.WAV"), []).unwrap();
        fs::write(temp.path().join("A.mp3"), []).unwrap();
        fs::write(temp.path().join("notes.txt"), []).unwrap();
        fs::create_dir(temp.path().join("folder.ogg")).unwrap();

        let files = discover(temp.path());
        let names: Vec<_> = files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy())
            .collect();
        assert_eq!(names, ["A.mp3", "z.WAV"]);
    }

    #[test]
    fn decodes_pcm_wav_to_normalized_samples() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tone.wav");
        fs::write(&path, mono_wav(&[0, i16::MAX], 8_000)).unwrap();

        let clip = decode(&path).unwrap();

        assert_eq!(clip.name.as_ref(), "tone");
        assert_eq!(clip.sample_rate, 8_000);
        assert_eq!(clip.channels, 1);
        assert_eq!(clip.samples.len(), 2);
        assert!(clip.samples[0].abs() < f32::EPSILON);
        assert!(clip.samples[1] > 0.99);
    }

    #[test]
    fn decodes_mp3_clip() {
        const MP3_HEX: &str = concat!(
            "49443304000000000023545353450000000f0000034c61766636322e31322e3130320000000000000000000000",
            "ffe338c4002b00d260015918015debbd77aef5dec4d9db96efb90d61862ec5d8bb19c390ee396ced61d40d41d",
            "75c8f8e1a60371305137693cf93f7d3e733a5d0536e2bf7002ec5d8ce1dc872c679cae371b97d39e780003830c",
            "ff000f7fc70c00740000f0f0f0f0c000000000f0f0f0f0c000000000f0f0f0f0c000000000f0f0f0f0c00000",
            "0000f0f0f0f0c000000000f0f0f0f0c000000000f0f0f0f0c000000000f0f0f0f0c000000101e1e1e1e9000",
            "0001c9c3c3dffc00200008844624118ae12ee47a693ffff565ff3ca025d552a2cf7fffc62f41cdffe338c41f34e",
            "bc27d199b78006cc2458280a7e0b068a59a710fc53058522ec1cf5af404660b101340ca4f885106ffffc0572e88",
            "c2f09f733749c9395321c7330a1bfffffe08315c2f02004acf31d65b10d6e572ba0bd6184f9f7ffffffe8c2685f",
            "d565ece458310e7736d56ab612b95d05ebd84f9f7ffffffff9e69c704627dcd1e9c88944fc782f5ed9f46cd6b8",
            "b5b35ffffffffffd6d5932c2be75b5669d2bf6ed59fffffff3fe3fafffffffffffffffe9a15fb725669c55fb725",
            "665c59f119d2c38002150c866331e00c8a224201836f6bf872617900ffe338c41632633285b9949800011030001",
            "132997871c061dd01e1cc00609748a3e0635072e1c686351700b113255325248fe067c00a3c062a0607836200928",
            "015016cecad7ff80d440c0f06c40125002a81bc6088a01070b800504ad7ab5ffe01240b9f0bb4030618a034a00d",
            "006470e04040432e065756bd5afffc1b7832f87801b921e8076c1b941fb87d02ce87e81f70b3c211ead7ab5ffff",
            "85ec00808710185c034018bc3040368863818cc1b4837617500df43240bab06fc1c362ec5d8ba4c414d45342e30",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaffe338c4170000034801c00000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tone.mp3");
        fs::write(&path, decode_hex(MP3_HEX)).unwrap();

        let clip = decode(&path).unwrap();

        assert_eq!(clip.name.as_ref(), "tone");
        assert_eq!(clip.sample_rate, 8_000);
        assert_eq!(clip.channels, 1);
        assert!(!clip.samples.is_empty());
        assert!(clip.duration_seconds() > 0.05);
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| nibble(pair[0]) << 4 | nibble(pair[1]))
            .collect()
    }

    const fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("invalid test hex"),
        }
    }

    fn mono_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
        let data_size = u32::try_from(std::mem::size_of_val(samples)).unwrap();
        let mut bytes = Vec::with_capacity(44 + data_size as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }
}
