use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    ToggleCameraFreeze,
    ToggleMicrophoneMute,
    PlayClip(PathBuf),
    StopClips,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    SetCameraFrozen(bool),
    SetMicrophoneMuted(bool),
    PlayClip(PathBuf),
    StopClips,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamState {
    pub camera_frozen: bool,
    pub microphone_muted: bool,
    pub active_clip: Option<PathBuf>,
}

impl StreamState {
    #[must_use]
    pub fn apply(&mut self, action: Action) -> Effect {
        match action {
            Action::ToggleCameraFreeze => {
                self.camera_frozen = !self.camera_frozen;
                Effect::SetCameraFrozen(self.camera_frozen)
            }
            Action::ToggleMicrophoneMute => {
                self.microphone_muted = !self.microphone_muted;
                Effect::SetMicrophoneMuted(self.microphone_muted)
            }
            Action::PlayClip(path) => {
                self.active_clip = Some(path.clone());
                Effect::PlayClip(path)
            }
            Action::StopClips => {
                self.active_clip = None;
                Effect::StopClips
            }
        }
    }

    #[must_use]
    pub const fn summary(&self) -> &'static str {
        match (self.camera_frozen, self.microphone_muted) {
            (false, false) => "Camera live, microphone live",
            (true, false) => "Camera frozen, microphone live",
            (false, true) => "Camera live, microphone muted",
            (true, true) => "Camera frozen, microphone muted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_are_independent_and_reversible() {
        let mut state = StreamState::default();

        assert_eq!(
            state.apply(Action::ToggleCameraFreeze),
            Effect::SetCameraFrozen(true)
        );
        assert_eq!(
            state.apply(Action::ToggleMicrophoneMute),
            Effect::SetMicrophoneMuted(true)
        );
        assert_eq!(
            state.apply(Action::ToggleCameraFreeze),
            Effect::SetCameraFrozen(false)
        );
        assert_eq!(state.summary(), "Camera live, microphone muted");
    }

    #[test]
    fn stopping_clears_active_clip() {
        let mut state = StreamState::default();
        let path = PathBuf::from("hello.wav");

        let _ = state.apply(Action::PlayClip(path));
        assert!(state.active_clip.is_some());
        assert_eq!(state.apply(Action::StopClips), Effect::StopClips);
        assert!(state.active_clip.is_none());
    }
}
