use std::f32::consts::{PI, TAU};

use serde::{Deserialize, Serialize};

const PARAMETER_SMOOTHING_MS: f32 = 20.0;
const EFFECT_CROSSFADE_MS: f32 = 25.0;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceEffect {
    #[default]
    Off,
    Deep,
    Robot,
    Anonymous,
    Radio,
    Alien,
    Tiny,
    Demon,
}

impl VoiceEffect {
    pub const ALL: [Self; 8] = [
        Self::Off,
        Self::Deep,
        Self::Robot,
        Self::Anonymous,
        Self::Radio,
        Self::Alien,
        Self::Tiny,
        Self::Demon,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Deep => "deep",
            Self::Robot => "robot",
            Self::Anonymous => "anonymous",
            Self::Radio => "radio",
            Self::Alien => "alien",
            Self::Tiny => "tiny",
            Self::Demon => "demon",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Clean",
            Self::Deep => "Deep",
            Self::Robot => "Robot",
            Self::Anonymous => "Anonymous",
            Self::Radio => "Radio",
            Self::Alien => "Alien",
            Self::Tiny => "Tiny",
            Self::Demon => "Demon",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| effect.id() == id)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VoiceEffectSettings {
    pub effect: VoiceEffect,
    pub intensity: f32,
    pub mix: f32,
}

impl Default for VoiceEffectSettings {
    fn default() -> Self {
        Self {
            effect: VoiceEffect::Off,
            intensity: 0.7,
            mix: 1.0,
        }
    }
}

impl VoiceEffectSettings {
    #[must_use]
    pub fn normalized(self) -> Self {
        let defaults = Self::default();
        Self {
            effect: self.effect,
            intensity: normalized_unit(self.intensity, defaults.intensity),
            mix: normalized_unit(self.mix, defaults.mix),
        }
    }
}

pub(super) struct VoiceProcessor {
    active_effect: VoiceEffect,
    previous_effect: VoiceEffect,
    active: EffectLane,
    previous: EffectLane,
    intensity: SmoothedValue,
    mix: SmoothedValue,
    previous_intensity: f32,
    transition_remaining: usize,
    transition_total: usize,
    smoothing_frames: usize,
    crossfade_frames: usize,
}

impl VoiceProcessor {
    pub(super) fn new(sample_rate: u32, settings: VoiceEffectSettings) -> Self {
        let settings = settings.normalized();
        let smoothing_frames = duration_frames(sample_rate, PARAMETER_SMOOTHING_MS);
        let crossfade_frames = duration_frames(sample_rate, EFFECT_CROSSFADE_MS);
        Self {
            active_effect: settings.effect,
            previous_effect: VoiceEffect::Off,
            active: EffectLane::new(sample_rate),
            previous: EffectLane::new(sample_rate),
            intensity: SmoothedValue::new(settings.intensity),
            mix: SmoothedValue::new(settings.mix),
            previous_intensity: settings.intensity,
            transition_remaining: 0,
            transition_total: crossfade_frames,
            smoothing_frames,
            crossfade_frames,
        }
    }

    pub(super) fn set_settings(&mut self, settings: VoiceEffectSettings) {
        let settings = settings.normalized();
        if settings.effect != self.active_effect {
            std::mem::swap(&mut self.active, &mut self.previous);
            self.previous_effect = self.active_effect;
            self.previous_intensity = self.intensity.current();
            self.active_effect = settings.effect;
            self.active.reset();
            self.transition_remaining = self.crossfade_frames;
            self.transition_total = self.crossfade_frames;
        }
        self.intensity
            .set_target(settings.intensity, self.smoothing_frames);
        self.mix.set_target(settings.mix, self.smoothing_frames);
    }

    pub(super) fn process_in_place(&mut self, samples: &mut [f32]) {
        if self.active_effect == VoiceEffect::Off
            && self.previous_effect == VoiceEffect::Off
            && self.transition_remaining == 0
        {
            self.intensity.finish();
            self.mix.finish();
            return;
        }

        for sample in samples {
            let dry = *sample;
            let intensity = self.intensity.next();
            let current = self.active.process(dry, self.active_effect, intensity);
            let wet = if self.transition_remaining == 0 {
                current
            } else {
                let previous =
                    self.previous
                        .process(dry, self.previous_effect, self.previous_intensity);
                let progress =
                    1.0 - self.transition_remaining as f32 / self.transition_total as f32;
                self.transition_remaining -= 1;
                if self.transition_remaining == 0 {
                    self.previous_effect = VoiceEffect::Off;
                }
                lerp(previous, current, smoothstep(progress))
            };
            *sample = lerp(dry, wet, self.mix.next()).clamp(-1.0, 1.0);
        }
    }
}

struct EffectLane {
    sample_rate: f32,
    pitch: PitchShifter,
    low_pass: OnePoleLowPass,
    high_pass: OnePoleHighPass,
    modulation_phase: f32,
}

impl EffectLane {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            pitch: PitchShifter::new(sample_rate),
            low_pass: OnePoleLowPass::new(sample_rate),
            high_pass: OnePoleHighPass::new(sample_rate),
            modulation_phase: 0.0,
        }
    }

    fn reset(&mut self) {
        self.pitch.reset();
        self.low_pass.reset();
        self.high_pass.reset();
        self.modulation_phase = 0.0;
    }

    fn process(&mut self, sample: f32, effect: VoiceEffect, intensity: f32) -> f32 {
        let intensity = intensity.clamp(0.0, 1.0);
        match effect {
            VoiceEffect::Off => sample,
            VoiceEffect::Deep => {
                let shifted = self.pitch.process(sample, lerp(1.0, 0.68, intensity));
                let darkened = self
                    .low_pass
                    .process(shifted, lerp(12_000.0, 3_800.0, intensity));
                let processed = soft_clip(darkened, lerp(1.0, 1.8, intensity));
                lerp(sample, processed, intensity)
            }
            VoiceEffect::Robot => {
                let carrier = self.oscillator(lerp(42.0, 118.0, intensity));
                let processed = sample * carrier;
                lerp(sample, processed, intensity)
            }
            VoiceEffect::Anonymous => {
                let shifted = self.pitch.process(sample, lerp(1.0, 0.78, intensity));
                let filtered = self.high_pass.process(
                    self.low_pass
                        .process(shifted, lerp(5_000.0, 2_400.0, intensity)),
                    lerp(100.0, 220.0, intensity),
                );
                let modulation = self.oscillator(lerp(16.0, 34.0, intensity));
                let modulated = filtered * lerp(1.0, modulation, 0.22 * intensity);
                let processed = soft_clip(modulated, lerp(1.0, 2.2, intensity));
                lerp(sample, processed, intensity)
            }
            VoiceEffect::Radio => {
                let filtered = self.high_pass.process(
                    self.low_pass
                        .process(sample, lerp(6_000.0, 2_500.0, intensity)),
                    lerp(120.0, 420.0, intensity),
                );
                let processed = soft_clip(filtered, lerp(1.0, 3.8, intensity));
                lerp(sample, processed, intensity)
            }
            VoiceEffect::Alien => {
                let shifted = self.pitch.process(sample, lerp(1.0, 1.45, intensity));
                let carrier = self.oscillator(lerp(15.0, 52.0, intensity));
                let processed = shifted * lerp(1.0, carrier, 0.55 * intensity);
                lerp(sample, processed, intensity)
            }
            VoiceEffect::Tiny => {
                let shifted = self.pitch.process(sample, lerp(1.0, 1.72, intensity));
                let processed = self
                    .high_pass
                    .process(shifted, lerp(60.0, 320.0, intensity));
                lerp(sample, processed, intensity)
            }
            VoiceEffect::Demon => {
                let shifted = self.pitch.process(sample, lerp(1.0, 0.55, intensity));
                let darkened = self
                    .low_pass
                    .process(shifted, lerp(8_000.0, 2_800.0, intensity));
                let carrier = self.oscillator(lerp(8.0, 21.0, intensity));
                let modulated = darkened * lerp(1.0, carrier, 0.18 * intensity);
                let processed = soft_clip(modulated, lerp(1.0, 3.2, intensity));
                lerp(sample, processed, intensity)
            }
        }
    }

    fn oscillator(&mut self, frequency: f32) -> f32 {
        let value = (self.modulation_phase * TAU).sin();
        self.modulation_phase = (self.modulation_phase + frequency / self.sample_rate).fract();
        value
    }
}

struct PitchShifter {
    buffer: Vec<f32>,
    mask: usize,
    write_index: usize,
    phase: f32,
    min_delay: f32,
    delay_range: f32,
}

impl PitchShifter {
    fn new(sample_rate: u32) -> Self {
        let sample_rate = usize::try_from(sample_rate).unwrap_or(48_000);
        let buffer_len = (sample_rate / 16).max(2_048).next_power_of_two();
        let min_delay = (sample_rate as f32 * 0.0025).max(24.0);
        let max_delay = (sample_rate as f32 * 0.035).min(buffer_len as f32 - 2.0);
        Self {
            buffer: vec![0.0; buffer_len],
            mask: buffer_len - 1,
            write_index: 0,
            phase: 0.0,
            min_delay,
            delay_range: max_delay - min_delay,
        }
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.write_index = 0;
        self.phase = 0.0;
    }

    fn process(&mut self, input: f32, ratio: f32) -> f32 {
        self.buffer[self.write_index] = input;

        let second_phase = (self.phase + 0.5).fract();
        let rising_delay = ratio <= 1.0;
        let first_delay = self.delay_for_phase(self.phase, rising_delay);
        let second_delay = self.delay_for_phase(second_phase, rising_delay);
        let first = self.read(first_delay);
        let second = self.read(second_delay);
        let first_weight = (PI * self.phase).sin().powi(2);
        let output = first.mul_add(first_weight, second * (1.0 - first_weight));

        let phase_step = (ratio - 1.0).abs() / self.delay_range;
        self.phase = (self.phase + phase_step).fract();
        self.write_index = (self.write_index + 1) & self.mask;
        output
    }

    fn delay_for_phase(&self, phase: f32, rising: bool) -> f32 {
        if rising {
            phase.mul_add(self.delay_range, self.min_delay)
        } else {
            (1.0 - phase).mul_add(self.delay_range, self.min_delay)
        }
    }

    fn read(&self, delay: f32) -> f32 {
        let mut position = self.write_index as f32 - delay;
        if position < 0.0 {
            position += self.buffer.len() as f32;
        }
        let before = position.floor() as usize & self.mask;
        let after = (before + 1) & self.mask;
        let fraction = position.fract();
        lerp(self.buffer[before], self.buffer[after], fraction)
    }
}

struct OnePoleLowPass {
    sample_rate: f32,
    cutoff: f32,
    coefficient: f32,
    state: f32,
}

impl OnePoleLowPass {
    const fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            cutoff: 0.0,
            coefficient: 0.0,
            state: 0.0,
        }
    }

    const fn reset(&mut self) {
        self.state = 0.0;
    }

    fn process(&mut self, input: f32, cutoff: f32) -> f32 {
        let cutoff = cutoff.clamp(20.0, self.sample_rate * 0.45);
        if (cutoff - self.cutoff).abs() >= 1.0 {
            self.cutoff = cutoff;
            self.coefficient = (-TAU * cutoff / self.sample_rate).exp();
        }
        self.state = input.mul_add(1.0 - self.coefficient, self.state * self.coefficient);
        self.state
    }
}

struct OnePoleHighPass {
    low_pass: OnePoleLowPass,
}

impl OnePoleHighPass {
    const fn new(sample_rate: u32) -> Self {
        Self {
            low_pass: OnePoleLowPass::new(sample_rate),
        }
    }

    const fn reset(&mut self) {
        self.low_pass.reset();
    }

    fn process(&mut self, input: f32, cutoff: f32) -> f32 {
        input - self.low_pass.process(input, cutoff)
    }
}

struct SmoothedValue {
    current: f32,
    target: f32,
    step: f32,
    remaining: usize,
}

impl SmoothedValue {
    const fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
        }
    }

    const fn current(&self) -> f32 {
        self.current
    }

    fn set_target(&mut self, target: f32, frames: usize) {
        self.target = target;
        if frames == 0 {
            self.finish();
            return;
        }
        self.step = (target - self.current) / frames as f32;
        self.remaining = frames;
    }

    fn next(&mut self) -> f32 {
        if self.remaining > 0 {
            self.current += self.step;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            }
        }
        self.current
    }

    const fn finish(&mut self) {
        self.current = self.target;
        self.step = 0.0;
        self.remaining = 0;
    }
}

fn duration_frames(sample_rate: u32, milliseconds: f32) -> usize {
    ((sample_rate as f32 * milliseconds / 1_000.0).round() as usize).max(1)
}

const fn normalized_unit(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

fn smoothstep(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * 2.0_f32.mul_add(-value, 3.0)
}

fn soft_clip(sample: f32, drive: f32) -> f32 {
    let driven = sample * drive;
    if driven <= -1.0 {
        -2.0 / 3.0
    } else if driven >= 1.0 {
        2.0 / 3.0
    } else {
        driven - driven.powi(3) / 3.0
    }
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    (to - from).mul_add(amount, from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(sample_rate: u32, frequency: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|frame| (TAU * frequency * frame as f32 / sample_rate as f32).sin() * 0.6)
            .collect()
    }

    #[test]
    fn effect_ids_round_trip() {
        for effect in VoiceEffect::ALL {
            assert_eq!(VoiceEffect::from_id(effect.id()), Some(effect));
            let json = serde_json::to_string(&effect).unwrap();
            assert_eq!(serde_json::from_str::<VoiceEffect>(&json).unwrap(), effect);
        }
        assert_eq!(VoiceEffect::from_id("unknown"), None);
    }

    #[test]
    fn settings_normalize_invalid_values() {
        assert_eq!(
            VoiceEffectSettings {
                effect: VoiceEffect::Robot,
                intensity: f32::NAN,
                mix: 4.0,
            }
            .normalized(),
            VoiceEffectSettings {
                effect: VoiceEffect::Robot,
                intensity: 0.7,
                mix: 1.0,
            }
        );
    }

    #[test]
    fn clean_effect_is_sample_exact() {
        let mut samples = sine(48_000, 440.0, 4_096);
        let expected = samples.clone();
        let mut processor = VoiceProcessor::new(48_000, VoiceEffectSettings::default());

        processor.process_in_place(&mut samples);

        assert_eq!(samples, expected);
    }

    #[test]
    fn zero_intensity_is_sample_exact_after_crossfade() {
        let settings = VoiceEffectSettings {
            effect: VoiceEffect::Demon,
            intensity: 0.0,
            mix: 1.0,
        };
        let mut processor = VoiceProcessor::new(48_000, settings);
        let mut samples = sine(48_000, 220.0, 4_096);
        let expected = samples.clone();

        processor.process_in_place(&mut samples);

        assert_eq!(samples, expected);
    }

    #[test]
    fn every_preset_produces_finite_bounded_audio() {
        for effect in VoiceEffect::ALL {
            let mut processor = VoiceProcessor::new(
                48_000,
                VoiceEffectSettings {
                    effect,
                    intensity: 0.8,
                    mix: 1.0,
                },
            );
            let mut samples = sine(48_000, 220.0, 12_000);
            processor.process_in_place(&mut samples);

            assert!(samples.iter().all(|sample| sample.is_finite()));
            assert!(samples.iter().all(|sample| sample.abs() <= 1.0));
            if effect != VoiceEffect::Off {
                assert_ne!(samples, sine(48_000, 220.0, 12_000));
            }
        }
    }

    #[test]
    fn preset_switch_crossfades_instead_of_jumping() {
        let mut processor = VoiceProcessor::new(48_000, VoiceEffectSettings::default());
        let mut warmup = vec![0.25; 4_096];
        processor.process_in_place(&mut warmup);
        processor.set_settings(VoiceEffectSettings {
            effect: VoiceEffect::Robot,
            intensity: 1.0,
            mix: 1.0,
        });
        let mut switched = vec![0.25; 1_500];

        processor.process_in_place(&mut switched);

        assert!((switched[0] - 0.25).abs() < 0.001);
        assert!((switched[1_499] - 0.25).abs() > 0.05);
    }

    #[test]
    fn switching_back_to_clean_restores_exact_bypass() {
        let mut processor = VoiceProcessor::new(
            48_000,
            VoiceEffectSettings {
                effect: VoiceEffect::Robot,
                intensity: 1.0,
                mix: 1.0,
            },
        );
        let mut warmup = vec![0.25; 2_000];
        processor.process_in_place(&mut warmup);
        processor.set_settings(VoiceEffectSettings::default());
        let mut crossfade = vec![0.25; 2_000];
        processor.process_in_place(&mut crossfade);
        let mut clean = sine(48_000, 440.0, 4_096);
        let expected = clean.clone();

        processor.process_in_place(&mut clean);

        assert_eq!(processor.active_effect, VoiceEffect::Off);
        assert_eq!(processor.previous_effect, VoiceEffect::Off);
        assert_eq!(clean, expected);
    }

    #[test]
    fn granular_shifter_retains_signal_after_warmup() {
        let mut shifter = PitchShifter::new(48_000);
        let input = sine(48_000, 440.0, 24_000);
        let output = input
            .into_iter()
            .map(|sample| shifter.process(sample, 0.7))
            .collect::<Vec<_>>();
        let tail = &output[4_000..];
        let rms =
            (tail.iter().map(|sample| sample * sample).sum::<f32>() / tail.len() as f32).sqrt();

        assert!(rms > 0.1);
        assert!(tail.iter().all(|sample| sample.is_finite()));
    }
}
