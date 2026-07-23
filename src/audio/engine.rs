use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use cpal::{
    Device, SampleFormat, Stream, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, bounded};
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use crate::{clips::DecodedClip, config::AppConfig};

use super::{
    effects::{VoiceEffect, VoiceEffectSettings, VoiceProcessor},
    mixer::{ClipMixer, MixSettings, MonoResampler},
};

const LOOPBACK_DEVICE_HINTS: &[&str] = &[
    "blackhole",
    "loopback",
    "soundflower",
    "vb-cable",
    "virtual cable",
    "static stream",
    "static microphone",
];
const STATIC_DEVICE: &str = "static microphone";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
    pub is_probable_loopback: bool,
}

#[derive(Clone, Debug)]
pub enum AudioCommand {
    SetMuted(bool),
    SetClipGain(f32),
    SetSpeakerGain(f32),
    SetVoiceEffect(VoiceEffectSettings),
    Play { request_id: u64, clip: DecodedClip },
    Stop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioLevels {
    pub clip: f32,
    pub physical_microphone: f32,
    pub processed_microphone: f32,
    pub virtual_microphone: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioEngineStatus {
    Ready {
        input: String,
        output: String,
        sample_rate: u32,
        channels: u16,
        speaker_monitor: Option<String>,
    },
    ClipStarted {
        request_id: u64,
        name: Arc<str>,
        duration_ms: u64,
    },
    ClipFinished {
        request_id: u64,
        name: Arc<str>,
    },
    ClipStopped {
        request_id: Option<u64>,
        name: Option<Arc<str>>,
    },
    VoiceEffectChanged(VoiceEffect),
    SpeakerMonitorError(String),
    StreamError(String),
}

#[derive(Default)]
struct SharedAudioLevels {
    clip: AtomicU32,
    physical_microphone: AtomicU32,
    processed_microphone: AtomicU32,
    virtual_microphone: AtomicU32,
}

impl SharedAudioLevels {
    fn observe(level: &AtomicU32, sample: f32) {
        let amplitude = sample.abs().clamp(0.0, 1.0);
        level.fetch_max(amplitude.to_bits(), Ordering::Relaxed);
    }

    fn take(&self) -> AudioLevels {
        AudioLevels {
            clip: f32::from_bits(self.clip.swap(0, Ordering::Relaxed)),
            physical_microphone: f32::from_bits(
                self.physical_microphone.swap(0, Ordering::Relaxed),
            ),
            processed_microphone: f32::from_bits(
                self.processed_microphone.swap(0, Ordering::Relaxed),
            ),
            virtual_microphone: f32::from_bits(self.virtual_microphone.swap(0, Ordering::Relaxed)),
        }
    }
}

pub struct AudioEngine {
    _input_stream: Stream,
    _output_stream: Stream,
    _speaker_stream: Option<Stream>,
    commands: Sender<AudioCommand>,
    statuses: Option<Receiver<AudioEngineStatus>>,
    ready: AudioEngineStatus,
    levels: Arc<SharedAudioLevels>,
}

impl AudioEngine {
    #[allow(clippy::too_many_lines)]
    pub fn start(config: &AppConfig) -> anyhow::Result<Self> {
        Self::start_with_progress(config, |_| {})
    }

    #[allow(clippy::too_many_lines)]
    pub fn start_with_progress(
        config: &AppConfig,
        mut progress: impl FnMut(String),
    ) -> anyhow::Result<Self> {
        progress("Discovering audio devices...".into());
        let host = cpal::default_host();
        let input = select_input(&host, config.input_device.as_deref())?;
        let output = select_output(&host, config.output_device.as_deref())?;
        let input_name = input.description()?.name().to_owned();
        let output_name = output.description()?.name().to_owned();
        progress(format!(
            "Preparing {input_name} and {output_name} at a shared sample rate..."
        ));
        let output_supported = output.default_output_config()?;
        require_f32("output", &output_supported)?;
        let output_config: StreamConfig = output_supported.into();
        let input_supported = matching_input_config(&input, output_config.sample_rate)?;
        require_f32("input", &input_supported)?;
        let input_config: StreamConfig = input_supported.into();
        let levels = Arc::new(SharedAudioLevels::default());

        let output_channels = output_config.channels;
        let latency_frames = usize::try_from(output_config.sample_rate).unwrap_or(48_000)
            * usize::from(config.audio_latency_ms)
            / 1_000;
        let ring_capacity = latency_frames.max(256) * 4;
        let ring = HeapRb::<f32>::new(ring_capacity);
        let (mut producer, mut consumer) = ring.split();
        for _ in 0..latency_frames {
            let _ = producer.try_push(0.0);
        }

        let (command_tx, command_rx) = bounded(32);
        let (status_tx, status_rx) = bounded(64);
        let input_channels = usize::from(input_config.channels);
        let input_channel_divisor = f32::from(input_config.channels);
        let mut input_resampler =
            MonoResampler::new(input_config.sample_rate, output_config.sample_rate);
        let input_status = status_tx.clone();
        let input_levels = Arc::clone(&levels);
        progress(format!("Opening physical microphone {input_name}..."));
        let input_stream = input.build_input_stream(
            input_config,
            move |data: &[f32], _| {
                let mut peak = 0.0_f32;
                for frame in data.chunks_exact(input_channels) {
                    let mono = frame.iter().copied().sum::<f32>() / input_channel_divisor;
                    peak = peak.max(mono.abs());
                    input_resampler.push(mono, |sample| {
                        let _ = producer.try_push(sample);
                    });
                }
                SharedAudioLevels::observe(&input_levels.physical_microphone, peak);
            },
            move |error| {
                let _ = input_status.try_send(AudioEngineStatus::StreamError(error.to_string()));
            },
            None,
        )?;
        progress(format!("Opening virtual microphone {output_name}..."));

        let settings = MixSettings {
            microphone_gain: config.microphone_gain,
            clip_gain: config.clip_gain,
            replace_microphone_while_playing: config.replace_microphone_while_playing,
            ..MixSettings::default()
        };
        let mut mixer = ClipMixer::new(output_config.sample_rate, output_channels, settings);
        let mut voice_processor =
            VoiceProcessor::new(output_config.sample_rate, config.voice_effect);
        let monitor_capacity = usize::try_from(output_config.sample_rate)
            .unwrap_or(48_000)
            .max(8_192)
            * 2;
        let monitor_ring = HeapRb::<f32>::new(monitor_capacity);
        let (mut monitor_producer, monitor_consumer) = monitor_ring.split();
        if config.play_clips_on_speakers {
            for _ in 0..latency_frames {
                let _ = monitor_producer.try_push(0.0);
            }
        }
        let mut microphone = vec![0.0; 8_192];
        let mut clip_monitor = vec![0.0; 8_192];
        let mut current_settings = settings;
        let speaker_gain = Arc::new(AtomicU32::new(config.speaker_gain.to_bits()));
        let command_speaker_gain = Arc::clone(&speaker_gain);
        let output_levels = Arc::clone(&levels);
        let speaker_monitor_enabled = config.play_clips_on_speakers;
        let playback_status = status_tx.clone();
        let output_status = status_tx.clone();
        let output_stream = output.build_output_stream(
            output_config,
            move |data: &mut [f32], _| {
                while let Ok(command) = command_rx.try_recv() {
                    apply_audio_command(
                        command,
                        &mut mixer,
                        &mut current_settings,
                        &mut voice_processor,
                        &command_speaker_gain,
                        &playback_status,
                    );
                }

                let samples_per_chunk = microphone.len() * usize::from(output_channels);
                for output_chunk in data.chunks_mut(samples_per_chunk) {
                    let frames = output_chunk.len() / usize::from(output_channels);
                    for sample in &mut microphone[..frames] {
                        *sample = consumer.try_pop().unwrap_or_default();
                    }

                    // Keep latency bounded if a source clock runs slightly faster.
                    while consumer.occupied_len() > ring_capacity * 3 / 4 {
                        let _ = consumer.try_pop();
                    }
                    voice_processor.process_in_place(&mut microphone[..frames]);
                    let processed_peak = microphone[..frames]
                        .iter()
                        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                    SharedAudioLevels::observe(&output_levels.processed_microphone, processed_peak);
                    let completed = mixer.render_with_clip_monitor(
                        output_chunk,
                        &microphone[..frames],
                        &mut clip_monitor[..frames],
                    );
                    let mut clip_peak = 0.0_f32;
                    for sample in &clip_monitor[..frames] {
                        clip_peak = clip_peak.max(sample.abs());
                        if speaker_monitor_enabled {
                            let _ = monitor_producer.try_push(*sample);
                        }
                    }
                    SharedAudioLevels::observe(&output_levels.clip, clip_peak);
                    let virtual_peak = output_chunk
                        .iter()
                        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                    SharedAudioLevels::observe(&output_levels.virtual_microphone, virtual_peak);
                    if let Some(completed) = completed {
                        let _ = playback_status.try_send(AudioEngineStatus::ClipFinished {
                            request_id: completed.request_id,
                            name: completed.name,
                        });
                    }
                }
            },
            move |error| {
                let _ = output_status.try_send(AudioEngineStatus::StreamError(error.to_string()));
            },
            Some(Duration::from_millis(u64::from(config.audio_latency_ms))),
        )?;

        let mut speaker_monitor_name = None;
        let speaker_stream = if config.play_clips_on_speakers {
            progress("Opening the default speakers for clip monitoring...".into());
            match build_speaker_monitor(
                &host,
                output_config.sample_rate,
                config.audio_latency_ms,
                monitor_consumer,
                &speaker_gain,
                &status_tx,
            ) {
                Ok((stream, name)) => {
                    speaker_monitor_name = Some(name);
                    Some(stream)
                }
                Err(error) => {
                    let _ = status_tx
                        .try_send(AudioEngineStatus::SpeakerMonitorError(error.to_string()));
                    None
                }
            }
        } else {
            None
        };

        progress("Starting audio streams...".into());
        input_stream.play()?;
        output_stream.play()?;
        if let Some(stream) = &speaker_stream {
            stream.play()?;
        }
        progress("Audio routing is ready.".into());

        let ready = AudioEngineStatus::Ready {
            input: input_name,
            output: output_name,
            sample_rate: output_config.sample_rate,
            channels: output_config.channels,
            speaker_monitor: speaker_monitor_name,
        };
        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            _speaker_stream: speaker_stream,
            commands: command_tx,
            statuses: Some(status_rx),
            ready,
            levels,
        })
    }

    #[must_use]
    pub fn command_sender(&self) -> Sender<AudioCommand> {
        self.commands.clone()
    }

    #[must_use]
    pub const fn ready_status(&self) -> &AudioEngineStatus {
        &self.ready
    }

    #[must_use]
    pub const fn take_status_receiver(&mut self) -> Option<Receiver<AudioEngineStatus>> {
        self.statuses.take()
    }

    #[must_use]
    pub fn take_levels(&self) -> AudioLevels {
        self.levels.take()
    }
}

fn build_speaker_monitor(
    host: &cpal::Host,
    sample_rate: u32,
    latency_ms: u16,
    mut consumer: HeapCons<f32>,
    gain: &Arc<AtomicU32>,
    statuses: &Sender<AudioEngineStatus>,
) -> anyhow::Result<(Stream, String)> {
    let speaker = select_speaker_output(host)?;
    let speaker_name = speaker.description()?.name().to_owned();
    let speaker_supported = matching_output_config(&speaker, sample_rate)?;
    require_f32("speaker output", &speaker_supported)?;
    let speaker_config: StreamConfig = speaker_supported.into();
    let speaker_channels = usize::from(speaker_config.channels);
    let speaker_error_status = statuses.clone();
    let stream_gain = Arc::clone(gain);
    let stream = speaker.build_output_stream(
        speaker_config,
        move |data: &mut [f32], _| {
            let gain = f32::from_bits(stream_gain.load(Ordering::Relaxed));
            for frame in data.chunks_exact_mut(speaker_channels) {
                let sample = (consumer.try_pop().unwrap_or_default() * gain).clamp(-1.0, 1.0);
                frame.fill(sample);
            }
        },
        move |error| {
            let _ = speaker_error_status
                .try_send(AudioEngineStatus::SpeakerMonitorError(error.to_string()));
        },
        Some(Duration::from_millis(u64::from(latency_ms))),
    )?;
    Ok((stream, speaker_name))
}

fn apply_audio_command(
    command: AudioCommand,
    mixer: &mut ClipMixer,
    settings: &mut MixSettings,
    voice_processor: &mut VoiceProcessor,
    speaker_gain: &AtomicU32,
    statuses: &Sender<AudioEngineStatus>,
) {
    match command {
        AudioCommand::SetMuted(muted) => {
            settings.microphone_muted = muted;
            mixer.set_settings(*settings);
        }
        AudioCommand::SetClipGain(gain) => {
            settings.clip_gain = gain.clamp(0.0, 2.0);
            mixer.set_settings(*settings);
        }
        AudioCommand::SetSpeakerGain(gain) => {
            speaker_gain.store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
        }
        AudioCommand::SetVoiceEffect(settings) => {
            let settings = settings.normalized();
            voice_processor.set_settings(settings);
            let _ = statuses.try_send(AudioEngineStatus::VoiceEffectChanged(settings.effect));
        }
        AudioCommand::Play { request_id, clip } => {
            let name = Arc::clone(&clip.name);
            let duration_ms = (clip.duration_seconds() * 1_000.0)
                .round()
                .clamp(0.0, u64::MAX as f64) as u64;
            if let Some(replaced) = mixer.play(request_id, clip) {
                let _ = statuses.try_send(AudioEngineStatus::ClipStopped {
                    request_id: Some(replaced.request_id),
                    name: Some(replaced.name),
                });
            }
            let _ = statuses.try_send(AudioEngineStatus::ClipStarted {
                request_id,
                name,
                duration_ms,
            });
        }
        AudioCommand::Stop => {
            let stopped = mixer.stop();
            let _ = statuses.try_send(AudioEngineStatus::ClipStopped {
                request_id: stopped.as_ref().map(|clip| clip.request_id),
                name: stopped.map(|clip| clip.name),
            });
        }
    }
}

pub fn list_devices() -> anyhow::Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default_input_id = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let default_output_id = host
        .default_output_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());

    let mut devices = Vec::new();
    for device in host.devices()? {
        let Ok(description) = device.description() else {
            continue;
        };
        let Ok(id) = device.id() else {
            continue;
        };
        let id = id.to_string();
        let name = description.name().to_owned();
        let lower_name = name.to_lowercase();
        devices.push(AudioDevice {
            is_input: device
                .supported_input_configs()
                .is_ok_and(|mut configs| configs.next().is_some()),
            is_output: device
                .supported_output_configs()
                .is_ok_and(|mut configs| configs.next().is_some()),
            is_probable_loopback: LOOPBACK_DEVICE_HINTS
                .iter()
                .any(|hint| lower_name.contains(hint)),
            id,
            name,
        });
    }

    devices.sort_by(|left, right| {
        let left_default = Some(&left.id) == default_input_id.as_ref()
            || Some(&left.id) == default_output_id.as_ref();
        let right_default = Some(&right.id) == default_input_id.as_ref()
            || Some(&right.id) == default_output_id.as_ref();
        right_default
            .cmp(&left_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(devices)
}

fn select_input(host: &cpal::Host, selected: Option<&str>) -> anyhow::Result<Device> {
    if let Some(selected) = selected.filter(|name| !is_loopback_name(name)) {
        return find_device(host, selected, true)
            .with_context(|| format!("input device `{selected}` is unavailable"));
    }
    if let Some(default) = host.default_input_device() {
        if default
            .description()
            .is_ok_and(|description| !is_loopback_name(description.name()))
        {
            return Ok(default);
        }
    }
    host.input_devices()?
        .find(|device| {
            device
                .description()
                .is_ok_and(|description| !is_loopback_name(description.name()))
        })
        .context("no physical microphone is available")
}

fn select_output(host: &cpal::Host, selected: Option<&str>) -> anyhow::Result<Device> {
    if let Some(selected) = selected {
        if !is_loopback_name(selected) {
            anyhow::bail!("output device `{selected}` is not a virtual microphone");
        }
        return find_device(host, selected, false)
            .with_context(|| format!("output device `{selected}` is unavailable"));
    }

    let devices = host.output_devices()?.collect::<Vec<_>>();
    if let Some(device) = devices.iter().find(|device| {
        device
            .description()
            .is_ok_and(|description| is_owned_output(description.name()))
    }) {
        return Ok(device.clone());
    }

    anyhow::bail!("Static Microphone is not installed; install it from Static Stream setup")
}

fn select_speaker_output(host: &cpal::Host) -> anyhow::Result<Device> {
    if let Some(default) = host.default_output_device() {
        if default
            .description()
            .is_ok_and(|description| !is_loopback_name(description.name()))
        {
            return Ok(default);
        }
    }
    host.output_devices()?
        .find(|device| {
            device
                .description()
                .is_ok_and(|description| !is_loopback_name(description.name()))
        })
        .context("no physical speaker output is available")
}

fn find_device(host: &cpal::Host, selected: &str, input: bool) -> Option<Device> {
    let devices = if input {
        host.input_devices().ok()?
    } else {
        host.output_devices().ok()?
    };
    devices.into_iter().find(|device| {
        device
            .description()
            .is_ok_and(|description| description.name() == selected)
    })
}

fn is_loopback_name(name: &str) -> bool {
    let lower_name = name.to_lowercase();
    LOOPBACK_DEVICE_HINTS
        .iter()
        .any(|hint| lower_name.contains(hint))
}

fn is_owned_output(name: &str) -> bool {
    name.eq_ignore_ascii_case(STATIC_DEVICE)
}

fn matching_input_config(
    device: &Device,
    sample_rate: u32,
) -> anyhow::Result<SupportedStreamConfig> {
    for range in device.supported_input_configs()? {
        if range.sample_format() == SampleFormat::F32
            && range.min_sample_rate() <= sample_rate
            && range.max_sample_rate() >= sample_rate
        {
            let config = range.with_sample_rate(sample_rate);
            return Ok(config);
        }
    }

    let fallback = device.default_input_config()?;
    require_f32("input", &fallback)?;
    Ok(fallback)
}

fn matching_output_config(
    device: &Device,
    sample_rate: u32,
) -> anyhow::Result<SupportedStreamConfig> {
    for range in device.supported_output_configs()? {
        if range.sample_format() == SampleFormat::F32
            && range.min_sample_rate() <= sample_rate
            && range.max_sample_rate() >= sample_rate
        {
            return Ok(range.with_sample_rate(sample_rate));
        }
    }
    anyhow::bail!("physical speakers do not support {sample_rate} Hz 32-bit float audio")
}

fn require_f32(kind: &str, config: &SupportedStreamConfig) -> anyhow::Result<()> {
    if config.sample_format() == SampleFormat::F32 {
        Ok(())
    } else {
        anyhow::bail!(
            "{kind} device uses unsupported sample format {:?}; 32-bit float is required",
            config.sample_format()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn clip() -> DecodedClip {
        DecodedClip {
            name: "notification".into(),
            samples: Arc::from([0.25_f32; 480]),
            sample_rate: 48_000,
            channels: 1,
        }
    }

    #[test]
    fn common_loopback_names_are_recognized_without_false_default() {
        for name in ["BlackHole 2ch", "Rogue Amoeba Loopback Audio", "VB-CABLE"] {
            let lower = name.to_lowercase();
            assert!(
                LOOPBACK_DEVICE_HINTS
                    .iter()
                    .any(|hint| lower.contains(hint))
            );
        }
        let lower = "MacBook Pro Speakers".to_lowercase();
        assert!(
            !LOOPBACK_DEVICE_HINTS
                .iter()
                .any(|hint| lower.contains(hint))
        );
    }

    #[test]
    fn automatic_output_name_is_only_the_owned_microphone() {
        assert!(is_owned_output("Static Microphone"));
        assert!(is_owned_output("static microphone"));
        assert!(!is_owned_output("BlackHole 2ch"));
        assert!(!is_owned_output("Loopback Audio"));
    }

    #[test]
    fn play_and_stop_commands_report_the_clip_request() {
        let (status_tx, status_rx) = bounded(4);
        let mut mixer = ClipMixer::new(48_000, 2, MixSettings::default());
        let mut settings = MixSettings::default();
        let mut voice_processor = VoiceProcessor::new(48_000, VoiceEffectSettings::default());
        let speaker_gain = AtomicU32::new(0.8_f32.to_bits());

        apply_audio_command(
            AudioCommand::Play {
                request_id: 42,
                clip: clip(),
            },
            &mut mixer,
            &mut settings,
            &mut voice_processor,
            &speaker_gain,
            &status_tx,
        );
        assert_eq!(
            status_rx.try_recv().unwrap(),
            AudioEngineStatus::ClipStarted {
                request_id: 42,
                name: "notification".into(),
                duration_ms: 10,
            }
        );

        apply_audio_command(
            AudioCommand::Stop,
            &mut mixer,
            &mut settings,
            &mut voice_processor,
            &speaker_gain,
            &status_tx,
        );
        assert_eq!(
            status_rx.try_recv().unwrap(),
            AudioEngineStatus::ClipStopped {
                request_id: Some(42),
                name: Some("notification".into()),
            }
        );
    }

    #[test]
    fn gain_commands_update_independent_outputs() {
        let (status_tx, _) = bounded(1);
        let mut settings = MixSettings::default();
        let mut mixer = ClipMixer::new(48_000, 2, settings);
        let mut voice_processor = VoiceProcessor::new(48_000, VoiceEffectSettings::default());
        let speaker_gain = AtomicU32::new(0.8_f32.to_bits());

        apply_audio_command(
            AudioCommand::SetClipGain(1.25),
            &mut mixer,
            &mut settings,
            &mut voice_processor,
            &speaker_gain,
            &status_tx,
        );
        apply_audio_command(
            AudioCommand::SetSpeakerGain(0.35),
            &mut mixer,
            &mut settings,
            &mut voice_processor,
            &speaker_gain,
            &status_tx,
        );

        assert_eq!(settings.clip_gain, 1.25);
        assert_eq!(f32::from_bits(speaker_gain.load(Ordering::Relaxed)), 0.35);
    }

    #[test]
    fn voice_effect_command_updates_processor_and_reports_activation() {
        let (status_tx, status_rx) = bounded(1);
        let mut settings = MixSettings::default();
        let mut mixer = ClipMixer::new(48_000, 2, settings);
        let mut voice_processor = VoiceProcessor::new(48_000, VoiceEffectSettings::default());
        let speaker_gain = AtomicU32::new(0.8_f32.to_bits());
        let voice_settings = VoiceEffectSettings {
            effect: VoiceEffect::Robot,
            intensity: 0.8,
            mix: 0.9,
        };

        apply_audio_command(
            AudioCommand::SetVoiceEffect(voice_settings),
            &mut mixer,
            &mut settings,
            &mut voice_processor,
            &speaker_gain,
            &status_tx,
        );

        assert_eq!(
            status_rx.try_recv().unwrap(),
            AudioEngineStatus::VoiceEffectChanged(VoiceEffect::Robot)
        );
    }

    #[test]
    fn audio_levels_return_peaks_and_reset_after_reading() {
        let levels = SharedAudioLevels::default();
        SharedAudioLevels::observe(&levels.clip, 0.25);
        SharedAudioLevels::observe(&levels.clip, -0.75);
        SharedAudioLevels::observe(&levels.physical_microphone, 0.5);

        assert_eq!(
            levels.take(),
            AudioLevels {
                clip: 0.75,
                physical_microphone: 0.5,
                processed_microphone: 0.0,
                virtual_microphone: 0.0,
            }
        );
        assert_eq!(levels.take(), AudioLevels::default());
    }
}
