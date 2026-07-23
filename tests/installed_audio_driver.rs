#![cfg(target_os = "macos")]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use cpal::{
    Device, SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use static_stream::{
    audio::{AudioCommand, AudioEngine, AudioEngineStatus, VoiceEffect, VoiceEffectSettings},
    clips::DecodedClip,
    config::AppConfig,
};

const DEVICE_NAME: &str = "Static Microphone";

#[test]
#[ignore = "requires the Static Stream Core Audio driver to be installed"]
fn installed_driver_loops_output_back_to_input() {
    let host = cpal::default_host();
    let device = static_microphone(&host);
    let output_supported = device.default_output_config().unwrap();
    let input_supported = device.default_input_config().unwrap();
    assert_eq!(output_supported.sample_format(), SampleFormat::F32);
    assert_eq!(input_supported.sample_format(), SampleFormat::F32);
    assert_eq!(
        output_supported.sample_rate(),
        input_supported.sample_rate()
    );

    let peak_bits = Arc::new(AtomicU32::new(0));
    let input_peak = Arc::clone(&peak_bits);
    let input = device
        .build_input_stream(
            input_supported.config(),
            move |data: &[f32], _| {
                for sample in data {
                    input_peak.fetch_max(sample.abs().to_bits(), Ordering::Relaxed);
                }
            },
            |error| panic!("Static Stream input failed: {error}"),
            None,
        )
        .unwrap();
    let output = device
        .build_output_stream(
            output_supported.config(),
            |data: &mut [f32], _| data.fill(0.25),
            |error| panic!("Static Stream output failed: {error}"),
            None,
        )
        .unwrap();

    output.play().unwrap();
    input.play().unwrap();
    std::thread::sleep(Duration::from_millis(750));

    let peak = f32::from_bits(peak_bits.load(Ordering::Relaxed));
    assert!(
        peak > 0.2,
        "expected looped-back audio above 0.2, measured {peak}"
    );
}

#[test]
#[ignore = "requires the Static Stream Core Audio driver to be installed"]
fn audio_engine_clip_reaches_static_microphone_input() {
    let host = cpal::default_host();
    let device = static_microphone(&host);
    let input_supported = device.default_input_config().unwrap();
    assert_eq!(input_supported.sample_format(), SampleFormat::F32);

    let peak_bits = Arc::new(AtomicU32::new(0));
    let input_peak = Arc::clone(&peak_bits);
    let input = device
        .build_input_stream(
            input_supported.config(),
            move |data: &[f32], _| {
                for sample in data {
                    input_peak.fetch_max(sample.abs().to_bits(), Ordering::Relaxed);
                }
            },
            |error| panic!("Static Microphone input failed: {error}"),
            None,
        )
        .unwrap();
    input.play().unwrap();

    let config = AppConfig {
        microphone_gain: 0.0,
        clip_gain: 1.0,
        replace_microphone_while_playing: true,
        play_clips_on_speakers: true,
        speaker_gain: 0.15,
        ..AppConfig::default()
    };
    let mut engine = AudioEngine::start_with_progress(&config, |message| {
        eprintln!("audio startup: {message}");
    })
    .unwrap();
    match engine.ready_status() {
        static_stream::audio::AudioEngineStatus::Ready {
            speaker_monitor: Some(name),
            ..
        } => assert!(!name.is_empty()),
        status => panic!("speaker monitoring did not start: {status:?}"),
    }
    let statuses = engine.take_status_receiver().unwrap();
    engine
        .command_sender()
        .try_send(AudioCommand::SetVoiceEffect(VoiceEffectSettings {
            effect: VoiceEffect::Robot,
            intensity: 0.8,
            mix: 1.0,
        }))
        .unwrap();
    let status = statuses
        .recv_timeout(Duration::from_millis(250))
        .expect("audio engine did not acknowledge the voice effect within 250 ms");
    assert_eq!(
        status,
        AudioEngineStatus::VoiceEffectChanged(VoiceEffect::Robot)
    );

    peak_bits.store(0, Ordering::Relaxed);
    let command_started = Instant::now();
    engine
        .command_sender()
        .try_send(AudioCommand::Play {
            request_id: 1,
            clip: DecodedClip {
                name: "integration tone".into(),
                samples: vec![0.4; 4_800].into(),
                sample_rate: 48_000,
                channels: 1,
            },
        })
        .unwrap();
    let status = statuses
        .recv_timeout(Duration::from_millis(250))
        .expect("audio engine did not acknowledge clip playback within 250 ms");
    assert!(
        matches!(status, AudioEngineStatus::ClipStarted { request_id: 1, .. }),
        "expected ClipStarted, received {status:?}"
    );
    eprintln!(
        "clip playback acknowledgement: {} ms",
        command_started.elapsed().as_millis()
    );
    std::thread::sleep(Duration::from_millis(500));

    let peak = f32::from_bits(peak_bits.load(Ordering::Relaxed));
    let levels = engine.take_levels();
    assert!(
        peak > 0.3,
        "expected mixed clip above 0.3 at Static Microphone input, measured {peak}"
    );
    assert!(
        levels.clip > 0.3,
        "clip meter did not observe the test tone"
    );
    assert!(
        levels.virtual_microphone > 0.3,
        "virtual microphone meter did not observe the test tone"
    );
}

fn static_microphone(host: &cpal::Host) -> Device {
    host.devices()
        .unwrap()
        .find(|device| {
            device
                .description()
                .is_ok_and(|description| description.name() == DEVICE_NAME)
        })
        .expect("Static Microphone is not installed")
}
