use std::sync::Arc;

use crate::clips::DecodedClip;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixSettings {
    pub microphone_gain: f32,
    pub clip_gain: f32,
    pub microphone_muted: bool,
    pub replace_microphone_while_playing: bool,
}

impl Default for MixSettings {
    fn default() -> Self {
        Self {
            microphone_gain: 1.0,
            clip_gain: 0.85,
            microphone_muted: false,
            replace_microphone_while_playing: false,
        }
    }
}

struct ActiveClip {
    request_id: u64,
    clip: DecodedClip,
    source_frame: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipCompletion {
    pub request_id: u64,
    pub name: Arc<str>,
}

pub struct ClipMixer {
    output_sample_rate: u32,
    output_channels: u16,
    active: Option<ActiveClip>,
    settings: MixSettings,
}

pub(super) struct MonoResampler {
    source_frames_per_output: f64,
    phase: f64,
    previous: Option<f32>,
}

impl MonoResampler {
    pub(crate) fn new(source_sample_rate: u32, output_sample_rate: u32) -> Self {
        Self {
            source_frames_per_output: f64::from(source_sample_rate) / f64::from(output_sample_rate),
            phase: 0.0,
            previous: None,
        }
    }

    pub(crate) fn push(&mut self, sample: f32, mut output: impl FnMut(f32)) {
        let Some(previous) = self.previous.replace(sample) else {
            return;
        };

        loop {
            if self.phase > 1.0 {
                break;
            }
            let value = (sample - previous).mul_add(self.phase as f32, previous);
            output(value);
            self.phase += self.source_frames_per_output;
        }
        self.phase -= 1.0;
    }
}

impl ClipMixer {
    #[must_use]
    pub const fn new(output_sample_rate: u32, output_channels: u16, settings: MixSettings) -> Self {
        Self {
            output_sample_rate,
            output_channels,
            active: None,
            settings,
        }
    }

    pub const fn set_settings(&mut self, settings: MixSettings) {
        self.settings = settings;
    }

    pub fn play(&mut self, request_id: u64, clip: DecodedClip) -> Option<ClipCompletion> {
        let replaced = self.stop();
        self.active = Some(ActiveClip {
            request_id,
            clip,
            source_frame: 0.0,
        });
        replaced
    }

    pub fn stop(&mut self) -> Option<ClipCompletion> {
        self.active.take().map(|active| ClipCompletion {
            request_id: active.request_id,
            name: active.clip.name,
        })
    }

    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.active.is_some()
    }

    pub fn render(
        &mut self,
        output: &mut [f32],
        microphone_mono: &[f32],
    ) -> Option<ClipCompletion> {
        self.render_internal(output, microphone_mono, None)
    }

    pub fn render_with_clip_monitor(
        &mut self,
        output: &mut [f32],
        microphone_mono: &[f32],
        clip_mono: &mut [f32],
    ) -> Option<ClipCompletion> {
        let frames = output.len() / usize::from(self.output_channels);
        assert!(
            clip_mono.len() >= frames,
            "clip monitor must contain one sample per output frame"
        );
        self.render_internal(output, microphone_mono, Some(clip_mono))
    }

    fn render_internal(
        &mut self,
        output: &mut [f32],
        microphone_mono: &[f32],
        mut clip_mono: Option<&mut [f32]>,
    ) -> Option<ClipCompletion> {
        let channel_count = usize::from(self.output_channels);
        let mut completed = None;
        assert!(channel_count > 0, "output must have at least one channel");
        assert_eq!(
            output.len() % channel_count,
            0,
            "output must contain whole frames"
        );

        for (frame_index, output_frame) in output.chunks_exact_mut(channel_count).enumerate() {
            if let Some(monitor) = &mut clip_mono {
                monitor[frame_index] = 0.0;
            }
            let clip_is_playing = self.active.as_ref().is_some_and(|active| {
                let total_frames = active.clip.samples.len() / usize::from(active.clip.channels);
                active.source_frame < total_frames as f64
            });
            let pass_microphone = !(self.settings.microphone_muted
                || self.settings.replace_microphone_while_playing && clip_is_playing);
            let microphone = microphone_mono
                .get(frame_index)
                .copied()
                .unwrap_or_default();
            let microphone = if pass_microphone {
                microphone * self.settings.microphone_gain
            } else {
                0.0
            };

            for sample in output_frame.iter_mut() {
                *sample = microphone;
            }

            let mut finished = false;
            if let Some(active) = &mut self.active {
                let total_frames = active.clip.samples.len() / usize::from(active.clip.channels);
                let frame = active.source_frame.floor() as usize;
                if frame >= total_frames {
                    finished = true;
                } else {
                    let fraction = (active.source_frame - frame as f64) as f32;
                    let mut monitor_sum = 0.0;
                    for (channel, sample) in output_frame.iter_mut().enumerate() {
                        let current = sample_for_channel(&active.clip, frame, channel);
                        let next = sample_for_channel(
                            &active.clip,
                            (frame + 1).min(total_frames - 1),
                            channel,
                        );
                        let clip_sample = (next - current).mul_add(fraction, current);
                        let clip_sample = clip_sample * self.settings.clip_gain;
                        monitor_sum += clip_sample;
                        *sample = (*sample + clip_sample).clamp(-1.0, 1.0);
                    }
                    if let Some(monitor) = &mut clip_mono {
                        monitor[frame_index] = monitor_sum / channel_count as f32;
                    }
                    active.source_frame +=
                        f64::from(active.clip.sample_rate) / f64::from(self.output_sample_rate);
                }
            }
            if finished {
                completed = self.stop();
            }
        }
        completed
    }
}

fn sample_for_channel(clip: &DecodedClip, frame: usize, output_channel: usize) -> f32 {
    let clip_channels = usize::from(clip.channels);
    if clip_channels == 1 {
        return clip.samples[frame];
    }

    let channel = output_channel.min(clip_channels - 1);
    clip.samples[frame * clip_channels + channel]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn clip(samples: &[f32], sample_rate: u32, channels: u16) -> DecodedClip {
        DecodedClip {
            name: "test".into(),
            samples: Arc::from(samples),
            sample_rate,
            channels,
        }
    }

    #[test]
    fn passes_microphone_to_every_output_channel() {
        let mut mixer = ClipMixer::new(48_000, 2, MixSettings::default());
        let mut output = [0.0; 4];

        let completed = mixer.render(&mut output, &[0.25, -0.5]);

        assert_eq!(output, [0.25, 0.25, -0.5, -0.5]);
        assert_eq!(completed, None);
    }

    #[test]
    fn mixes_mono_clip_and_clamps_output() {
        let mut mixer = ClipMixer::new(
            48_000,
            2,
            MixSettings {
                clip_gain: 1.0,
                ..MixSettings::default()
            },
        );
        assert_eq!(mixer.play(1, clip(&[0.8, -0.25], 48_000, 1)), None);
        let mut output = [0.0; 4];

        let completed = mixer.render(&mut output, &[0.5, 0.0]);

        assert_eq!(output, [1.0, 1.0, -0.25, -0.25]);
        assert_eq!(completed, None);
    }

    #[test]
    fn reports_clip_audio_separately_from_microphone_mix() {
        let mut mixer = ClipMixer::new(
            48_000,
            2,
            MixSettings {
                clip_gain: 0.5,
                ..MixSettings::default()
            },
        );
        assert_eq!(mixer.play(1, clip(&[0.8], 48_000, 1)), None);
        let mut output = [0.0; 4];
        let mut clip_monitor = [0.0; 2];

        let completed = mixer.render_with_clip_monitor(&mut output, &[0.2, 0.2], &mut clip_monitor);

        assert_eq!(output, [0.6, 0.6, 0.2, 0.2]);
        assert_eq!(clip_monitor, [0.4, 0.0]);
        assert!(completed.is_some());
    }

    #[test]
    fn replacement_mode_suppresses_microphone_only_during_clip() {
        let settings = MixSettings {
            replace_microphone_while_playing: true,
            ..MixSettings::default()
        };
        let mut mixer = ClipMixer::new(48_000, 1, settings);
        assert_eq!(mixer.play(7, clip(&[0.25], 48_000, 1)), None);
        let mut output = [0.0; 2];

        let completed = mixer.render(&mut output, &[0.75, 0.75]);

        assert_eq!(output, [0.2125, 0.75]);
        assert!(!mixer.is_playing());
        assert_eq!(
            completed,
            Some(ClipCompletion {
                request_id: 7,
                name: "test".into(),
            })
        );
    }

    #[test]
    fn linearly_resamples_clip() {
        let mut mixer = ClipMixer::new(
            4,
            1,
            MixSettings {
                clip_gain: 1.0,
                microphone_muted: true,
                ..MixSettings::default()
            },
        );
        assert_eq!(mixer.play(1, clip(&[0.0, 1.0], 2, 1)), None);
        let mut output = [0.0; 4];

        let completed = mixer.render(&mut output, &[]);

        assert_eq!(output, [0.0, 0.5, 1.0, 1.0]);
        assert_eq!(completed, None);
    }

    #[test]
    fn replacing_and_stopping_report_the_affected_request() {
        let mut mixer = ClipMixer::new(48_000, 1, MixSettings::default());
        assert_eq!(mixer.play(11, clip(&[0.25], 48_000, 1)), None);

        let replaced = mixer.play(12, clip(&[0.5], 48_000, 1));
        assert_eq!(
            replaced,
            Some(ClipCompletion {
                request_id: 11,
                name: "test".into(),
            })
        );
        assert_eq!(
            mixer.stop(),
            Some(ClipCompletion {
                request_id: 12,
                name: "test".into(),
            })
        );
    }

    #[test]
    fn microphone_resampler_upsamples_streaming_input() {
        let mut resampler = MonoResampler::new(2, 4);
        let mut output = Vec::new();
        for sample in [0.0, 1.0, 0.0] {
            resampler.push(sample, |value| output.push(value));
        }

        assert_eq!(output, [0.0, 0.5, 1.0, 0.5, 0.0]);
    }

    #[test]
    fn microphone_resampler_downsamples_streaming_input() {
        let mut resampler = MonoResampler::new(4, 2);
        let mut output = Vec::new();
        for sample in [0.0, 0.5, 1.0, 0.5] {
            resampler.push(sample, |value| output.push(value));
        }

        assert_eq!(output, [0.0, 1.0]);
    }
}
