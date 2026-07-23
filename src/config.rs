use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{APP_NAME, audio::VoiceEffectSettings};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub clip_gain: f32,
    pub play_clips_on_speakers: bool,
    pub speaker_gain: f32,
    pub microphone_gain: f32,
    pub replace_microphone_while_playing: bool,
    pub audio_latency_ms: u16,
    pub voice_effect: VoiceEffectSettings,
    pub auto_check_updates: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            clip_gain: 0.85,
            play_clips_on_speakers: false,
            speaker_gain: 0.8,
            microphone_gain: 1.0,
            replace_microphone_while_playing: false,
            audio_latency_ms: 40,
            voice_effect: VoiceEffectSettings::default(),
            auto_check_updates: true,
        }
    }
}

impl AppConfig {
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.clip_gain = self.clip_gain.clamp(0.0, 2.0);
        self.speaker_gain = self.speaker_gain.clamp(0.0, 2.0);
        self.microphone_gain = self.microphone_gain.clamp(0.0, 2.0);
        self.audio_latency_ms = self.audio_latency_ms.clamp(10, 250);
        self.voice_effect = self.voice_effect.normalized();
        self
    }
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub clips_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> anyhow::Result<Self> {
        let project = ProjectDirs::from("com", "MadPin", APP_NAME)
            .ok_or_else(|| anyhow::anyhow!("could not resolve application data directory"))?;
        Ok(Self::from_config_dir(project.config_dir().to_path_buf()))
    }

    #[must_use]
    pub fn from_config_dir(config_dir: PathBuf) -> Self {
        Self {
            config_file: config_dir.join("config.json"),
            clips_dir: config_dir.join("Clips"),
            config_dir,
        }
    }

    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.clips_dir)
    }

    pub fn load(&self) -> anyhow::Result<AppConfig> {
        match fs::read(&self.config_file) {
            Ok(bytes) => Ok(serde_json::from_slice::<AppConfig>(&bytes)?.normalized()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AppConfig::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, config: &AppConfig) -> anyhow::Result<()> {
        self.ensure()?;
        let bytes = serde_json::to_vec_pretty(&config.clone().normalized())?;
        atomic_write(&self.config_file, &bytes)?;
        Ok(())
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;

    let temporary = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_defaults_and_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_config_dir(temp.path().to_path_buf());
        assert_eq!(paths.load().unwrap(), AppConfig::default());

        let config = AppConfig {
            input_device: Some("Studio Mic".into()),
            clip_gain: 0.5,
            ..AppConfig::default()
        };
        paths.save(&config).unwrap();

        assert_eq!(paths.load().unwrap(), config);
        assert!(paths.clips_dir.is_dir());
    }

    #[test]
    fn invalid_ranges_are_normalized() {
        let config = AppConfig {
            clip_gain: 5.0,
            speaker_gain: 4.0,
            microphone_gain: -1.0,
            audio_latency_ms: 1,
            voice_effect: VoiceEffectSettings {
                intensity: f32::NAN,
                mix: 5.0,
                ..VoiceEffectSettings::default()
            },
            ..AppConfig::default()
        }
        .normalized();

        assert_eq!(config.clip_gain, 2.0);
        assert_eq!(config.speaker_gain, 2.0);
        assert_eq!(config.microphone_gain, 0.0);
        assert_eq!(config.audio_latency_ms, 10);
        assert_eq!(config.voice_effect.intensity, 0.7);
        assert_eq!(config.voice_effect.mix, 1.0);
    }
}
