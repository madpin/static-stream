use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::Context;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use objc2::{AnyThread, rc::Retained};
use objc2_foundation::{NSString, NSUserDefaults};
use serde::{Deserialize, Serialize};
use static_stream::{
    APP_GROUP, APP_NAME,
    audio::{
        AudioCommand, AudioDevice, AudioEngine, AudioEngineStatus, AudioLevels, VoiceEffect,
        VoiceEffectSettings, list_devices,
    },
    clips::{self, DecodedClip},
    config::{AppConfig, AppPaths},
    state::{Action, Effect, StreamState},
    updates::{self, StagedUpdate, UpdateManifest},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    platform::macos::{ActivationPolicy, EventLoopExtMacOS},
    window::{Window, WindowBuilder},
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{
        CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
        accelerator::{Accelerator, Code as MenuCode, Modifiers as MenuModifiers},
    },
};
use wry::{WebView, WebViewBuilder};

const APP_HTML: &str = include_str!("../assets/app.html");
const MENU_OPEN_WINDOW: &str = "window.open";
const MENU_CAMERA: &str = "camera.freeze";
const MENU_MICROPHONE: &str = "microphone.mute";
const MENU_REPLACE_MICROPHONE: &str = "microphone.replace";
const MENU_CYCLE_VOICE_EFFECT: &str = "voice-effect.cycle";
const MENU_VOICE_EFFECT_PREFIX: &str = "voice-effect.";
const MENU_STOP: &str = "clips.stop";
const MENU_INSTALL_CAMERA: &str = "camera.install";
const MENU_INSTALL_AUDIO: &str = "audio.install";
const MENU_UNINSTALL_CAMERA: &str = "camera.uninstall";
const MENU_UNINSTALL_AUDIO: &str = "audio.uninstall";
const MENU_OPEN_CLIPS: &str = "clips.open";
const MENU_REFRESH: &str = "refresh";
const MENU_CHECK_UPDATES: &str = "updates.check";
const MENU_QUIT: &str = "quit";
const CAMERA_FROZEN_KEY: &str = "cameraFrozen";
const CAMERA_EXTENSION_ID: &str = "com.madpin.staticstream.camera";
const STATIC_MICROPHONE: &str = "Static Microphone";
const LEGACY_STATIC_MICROPHONE: &str = "Static Stream Microphone";
const MAX_ACTIVITY_EVENTS: usize = 250;
const AUDIO_STARTUP_WARNING_DELAY: Duration = Duration::from_secs(8);
const AUTO_UPDATE_CHECK_DELAY: Duration = Duration::from_secs(5);
const VISIBLE_TELEMETRY_INTERVAL: Duration = Duration::from_millis(100);
const HIDDEN_TELEMETRY_INTERVAL: Duration = Duration::from_millis(500);
const AUDIO_DRIVER_PATHS: [&str; 3] = [
    "/Library/Audio/Plug-Ins/HAL/StaticStreamAudio.driver",
    "/Library/Audio/Plug-Ins/HAL/Static Stream Audio.driver",
    "/Library/Audio/Plug-Ins/HAL/StaticStream.driver",
];
const TEAM_IDENTIFIER_PREFIX: &str = match option_env!("STATIC_STREAM_TEAM_PREFIX") {
    Some(prefix) => prefix,
    None => "",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceOperation {
    Install,
    Uninstall,
}

impl DeviceOperation {
    const fn command(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Install => "install/update",
            Self::Uninstall => "uninstall",
        }
    }
}

enum UserEvent {
    Menu(MenuEvent),
    HotKey(GlobalHotKeyEvent),
    Gui(String),
    ClipDecoded {
        request_id: u64,
        result: Result<DecodedClip, String>,
    },
    AudioStarted {
        request_id: u64,
        result: Result<AudioEngine, String>,
    },
    AudioStatus {
        request_id: u64,
        status: AudioEngineStatus,
    },
    AudioProgress {
        request_id: u64,
        message: String,
    },
    CameraProbe(Result<CameraProbe, String>),
    CameraDevelopmentTestFinished(Result<String, String>),
    CameraOperationFinished(DeviceOperation, Result<String, String>),
    AudioOperationFinished(DeviceOperation, Result<String, String>),
    UpdateCheckFinished(Result<Option<UpdateManifest>, String>),
    UpdateStaged(Result<StagedUpdate, String>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CameraProbe {
    static_stream_camera_available: bool,
    #[serde(default)]
    extension_registered: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
enum GuiCommand {
    Ready,
    SetCamera { frozen: bool },
    SetMuted { muted: bool },
    SetReplace { enabled: bool },
    SetClipGain { gain: f32 },
    SetVoiceEffect { effect: VoiceEffect },
    SetVoiceIntensity { intensity: f32 },
    SetVoiceMix { mix: f32 },
    SetSpeakerMonitor { enabled: bool },
    SetSpeakerGain { gain: f32 },
    SelectInput { name: String },
    SelectOutput { name: String },
    PlayClip { index: usize },
    StopClip,
    OpenClips,
    RefreshClips,
    Refresh,
    TestCamera,
    InstallCamera,
    InstallAudio,
    UninstallCamera,
    UninstallAudio,
    SetAutoCheckUpdates { enabled: bool },
    CheckForUpdates,
    InstallUpdate,
    ClearActivity,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum ActivityLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ClipPhase {
    Idle,
    Loading,
    Starting,
    Playing,
    Finished,
    Stopped,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityEvent {
    id: u64,
    elapsed_ms: u64,
    level: ActivityLevel,
    category: String,
    message: String,
}

struct ActivityLog {
    started_at: Instant,
    next_id: u64,
    events: VecDeque<ActivityEvent>,
}

impl ActivityLog {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            next_id: 1,
            events: VecDeque::with_capacity(MAX_ACTIVITY_EVENTS),
        }
    }

    fn push(
        &mut self,
        level: ActivityLevel,
        category: impl Into<String>,
        message: impl Into<String>,
    ) {
        if self.events.len() == MAX_ACTIVITY_EVENTS {
            let _ = self.events.pop_front();
        }
        self.events.push_back(ActivityEvent {
            id: self.next_id,
            elapsed_ms: self
                .started_at
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            level,
            category: category.into(),
            message: message.into(),
        });
        self.next_id = self.next_id.saturating_add(1);
    }

    fn snapshot(&self) -> Vec<ActivityEvent> {
        self.events.iter().rev().cloned().collect()
    }

    fn clear(&mut self) {
        self.events.clear();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct GuiState {
    camera_available: bool,
    camera_frozen: bool,
    camera_extension_installed: bool,
    camera_installer_available: bool,
    camera_signing_ready: bool,
    camera_message: String,
    camera_setup_detail: String,
    camera_test_available: bool,
    camera_test_busy: bool,
    camera_test_message: String,
    audio_driver_available: bool,
    audio_driver_installed: bool,
    audio_ready: bool,
    audio_installer_available: bool,
    audio_message: String,
    audio_setup_detail: String,
    microphone_muted: bool,
    replace_microphone: bool,
    clip_gain: f32,
    voice_effect: VoiceEffect,
    voice_effect_intensity: f32,
    voice_effect_mix: f32,
    play_clips_on_speakers: bool,
    speaker_gain: f32,
    speaker_monitor_ready: bool,
    speaker_monitor_message: String,
    audio_levels: GuiAudioLevels,
    selected_input: Option<String>,
    selected_output: Option<String>,
    input_devices: Vec<GuiNamedItem>,
    output_devices: Vec<GuiNamedItem>,
    clips: Vec<GuiNamedItem>,
    clip_phase: ClipPhase,
    clip_name: Option<String>,
    clip_message: String,
    clip_progress: f32,
    current_version: &'static str,
    auto_check_updates: bool,
    update_checking: bool,
    update_installing: bool,
    update_install_supported: bool,
    update_available_version: Option<String>,
    update_notes: String,
    update_status: String,
    activity_events: Vec<ActivityEvent>,
    busy: bool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct GuiTelemetry {
    audio_levels: GuiAudioLevels,
    clip_progress: f32,
}

#[derive(Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct GuiAudioLevels {
    clip: f32,
    physical_microphone: f32,
    processed_microphone: f32,
    virtual_microphone: f32,
}

impl From<AudioLevels> for GuiAudioLevels {
    fn from(levels: AudioLevels) -> Self {
        Self {
            clip: levels.clip,
            physical_microphone: levels.physical_microphone,
            processed_microphone: levels.processed_microphone,
            virtual_microphone: levels.virtual_microphone,
        }
    }
}

#[derive(Serialize)]
struct GuiNamedItem {
    name: String,
}

#[derive(Clone, Copy, Debug)]
enum ShortcutAction {
    ToggleCamera,
    ToggleMicrophone,
    CycleVoiceEffect,
    StopClips,
    PlayClip(usize),
}

#[derive(Clone, Debug)]
enum DeviceSelection {
    Input(String),
    Output(String),
}

struct MenuUi {
    status: MenuItem,
    check_updates: MenuItem,
    camera: CheckMenuItem,
    microphone: CheckMenuItem,
    replace_microphone: CheckMenuItem,
    voice_effect_items: Vec<(CheckMenuItem, VoiceEffect)>,
    input_items: Vec<(CheckMenuItem, String)>,
    output_items: Vec<(CheckMenuItem, String)>,
    selections: HashMap<String, DeviceSelection>,
}

struct DeviceMenus {
    input: Submenu,
    output: Submenu,
    input_items: Vec<(CheckMenuItem, String)>,
    output_items: Vec<(CheckMenuItem, String)>,
    selections: HashMap<String, DeviceSelection>,
}

#[allow(clippy::struct_excessive_bools)]
struct Controller {
    paths: AppPaths,
    config: AppConfig,
    state: StreamState,
    clips: Vec<PathBuf>,
    devices: Vec<AudioDevice>,
    audio: Option<AudioEngine>,
    audio_error: Option<String>,
    audio_levels: AudioLevels,
    speaker_monitor_error: Option<String>,
    next_audio_request_id: u64,
    active_audio_request: Option<u64>,
    current_audio_request: Option<u64>,
    audio_startup_warning: Option<(u64, Instant)>,
    clip_phase: ClipPhase,
    clip_name: Option<String>,
    clip_message: String,
    clip_started_at: Option<Instant>,
    clip_duration: Option<Duration>,
    next_clip_request_id: u64,
    active_clip_request: Option<u64>,
    camera_available: bool,
    camera_extension_registered: bool,
    camera_signing_ready: bool,
    camera_status: Option<String>,
    camera_test_busy: bool,
    camera_test_status: Option<String>,
    installer_busy: bool,
    update_checking: bool,
    update_installing: bool,
    update_install_unavailable_reason: Option<String>,
    update_available: Option<UpdateManifest>,
    update_status: String,
    activity: ActivityLog,
    tray: Option<TrayIcon>,
    ui: Option<MenuUi>,
    window: Option<Window>,
    webview: Option<WebView>,
    shared_defaults: Option<Retained<NSUserDefaults>>,
    proxy: EventLoopProxy<UserEvent>,
}

pub fn run() -> anyhow::Result<()> {
    let paths = AppPaths::discover()?;
    paths.ensure()?;
    let config = paths.load().unwrap_or_else(|error| {
        tracing::warn!("could not load config: {error}");
        AppConfig::default()
    });

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();

    install_event_forwarders(&proxy);
    let (hotkey_manager, shortcuts) = register_hotkeys()?;

    let mut controller = Controller::new(paths, config, proxy);
    let mut next_telemetry_at = Instant::now();
    event_loop.run(move |event, event_loop, control_flow| {
        let _keep_hotkeys_alive = &hotkey_manager;

        match event {
            Event::NewEvents(StartCause::Init) => {
                if let Err(error) = controller.create_shell(event_loop) {
                    tracing::error!("failed to create Static Stream UI: {error:#}");
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                controller.start_audio_async();
                if controller.config.auto_check_updates {
                    controller.check_for_updates(AUTO_UPDATE_CHECK_DELAY);
                }
            }
            Event::UserEvent(UserEvent::Menu(event)) => {
                if controller.handle_menu(event.id.as_ref()) {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            Event::UserEvent(UserEvent::HotKey(event)) if event.state == HotKeyState::Pressed => {
                if let Some(action) = shortcuts.get(&event.id) {
                    controller.handle_shortcut(*action);
                }
            }
            Event::UserEvent(UserEvent::Gui(message)) => {
                controller.handle_gui(&message);
            }
            Event::UserEvent(UserEvent::ClipDecoded { request_id, result }) => {
                controller.handle_decoded_clip(request_id, result);
            }
            Event::UserEvent(UserEvent::AudioStarted { request_id, result }) => {
                controller.handle_audio_started(request_id, result);
            }
            Event::UserEvent(UserEvent::AudioStatus { request_id, status }) => {
                controller.handle_audio_status_event(request_id, status);
            }
            Event::UserEvent(UserEvent::AudioProgress {
                request_id,
                message,
            }) => {
                controller.handle_audio_progress(request_id, message);
            }
            Event::UserEvent(UserEvent::CameraProbe(result)) => {
                controller.handle_camera_probe(result);
            }
            Event::UserEvent(UserEvent::CameraDevelopmentTestFinished(result)) => {
                controller.handle_camera_development_test(result);
            }
            Event::UserEvent(UserEvent::CameraOperationFinished(operation, result)) => {
                controller.handle_camera_operation(operation, result);
            }
            Event::UserEvent(UserEvent::AudioOperationFinished(operation, result)) => {
                controller.handle_audio_operation(operation, result);
            }
            Event::UserEvent(UserEvent::UpdateCheckFinished(result)) => {
                controller.handle_update_check(result);
            }
            Event::UserEvent(UserEvent::UpdateStaged(result)) => {
                if controller.handle_staged_update(result) {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => controller.hide_window(),
            _ => {}
        }

        let now = Instant::now();
        if now >= next_telemetry_at {
            controller.poll_audio_telemetry();
            next_telemetry_at = now + controller.telemetry_interval();
        } else {
            next_telemetry_at = next_telemetry_at.min(now + controller.telemetry_interval());
        }
        *control_flow = ControlFlow::WaitUntil(next_telemetry_at);
    });
}

fn install_event_forwarders(proxy: &EventLoopProxy<UserEvent>) {
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let hotkey_proxy = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event| {
        let _ = hotkey_proxy.send_event(UserEvent::HotKey(event));
    }));
}

fn register_hotkeys() -> anyhow::Result<(GlobalHotKeyManager, HashMap<u32, ShortcutAction>)> {
    let manager = GlobalHotKeyManager::new()?;
    let modifiers = Modifiers::SUPER | Modifiers::ALT;
    let bindings = [
        (
            HotKey::new(Some(modifiers), Code::KeyF),
            ShortcutAction::ToggleCamera,
        ),
        (
            HotKey::new(Some(modifiers), Code::KeyM),
            ShortcutAction::ToggleMicrophone,
        ),
        (
            HotKey::new(Some(modifiers), Code::KeyV),
            ShortcutAction::CycleVoiceEffect,
        ),
        (
            HotKey::new(Some(modifiers), Code::KeyX),
            ShortcutAction::StopClips,
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit1),
            ShortcutAction::PlayClip(0),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit2),
            ShortcutAction::PlayClip(1),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit3),
            ShortcutAction::PlayClip(2),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit4),
            ShortcutAction::PlayClip(3),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit5),
            ShortcutAction::PlayClip(4),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit6),
            ShortcutAction::PlayClip(5),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit7),
            ShortcutAction::PlayClip(6),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit8),
            ShortcutAction::PlayClip(7),
        ),
        (
            HotKey::new(Some(modifiers), Code::Digit9),
            ShortcutAction::PlayClip(8),
        ),
    ];

    let mut actions = HashMap::new();
    for (hotkey, action) in bindings {
        manager
            .register(hotkey)
            .with_context(|| format!("could not register global shortcut {hotkey}"))?;
        actions.insert(hotkey.id(), action);
    }
    Ok((manager, actions))
}

impl Controller {
    fn new(paths: AppPaths, config: AppConfig, proxy: EventLoopProxy<UserEvent>) -> Self {
        let clips = clips::discover(&paths.clips_dir);
        let devices = list_devices().unwrap_or_else(|error| {
            tracing::warn!("could not list audio devices: {error}");
            Vec::new()
        });
        let app_group = format!("{TEAM_IDENTIFIER_PREFIX}{APP_GROUP}");
        let shared_defaults = NSUserDefaults::initWithSuiteName(
            NSUserDefaults::alloc(),
            Some(&NSString::from_str(&app_group)),
        );
        let state = StreamState {
            camera_frozen: shared_defaults.as_ref().is_some_and(|defaults| {
                defaults.boolForKey(&NSString::from_str(CAMERA_FROZEN_KEY))
            }),
            ..StreamState::default()
        };
        let mut activity = ActivityLog::new();
        activity.push(
            ActivityLevel::Info,
            "App",
            format!(
                "Controller started with {} audio devices and {} sound clips.",
                devices.len(),
                clips.len()
            ),
        );
        activity.push(
            ActivityLevel::Info,
            "Audio",
            "Audio routing will start after the control shell is ready.",
        );
        let update_install_unavailable_reason = updates::installation_support()
            .err()
            .map(|error| error.to_string());
        let update_status = update_install_unavailable_reason.as_ref().map_or_else(
            || "Updates have not been checked yet.".into(),
            |reason| {
                format!(
                    "Updates can be checked, but automatic installation is unavailable: {reason}."
                )
            },
        );

        Self {
            paths,
            config,
            state,
            clips,
            devices,
            audio: None,
            audio_error: Some("Starting audio routing...".into()),
            audio_levels: AudioLevels::default(),
            speaker_monitor_error: None,
            next_audio_request_id: 1,
            active_audio_request: None,
            current_audio_request: None,
            audio_startup_warning: None,
            clip_phase: ClipPhase::Idle,
            clip_name: None,
            clip_message: "Select a clip to play it through the virtual microphone.".into(),
            clip_started_at: None,
            clip_duration: None,
            next_clip_request_id: 1,
            active_clip_request: None,
            camera_available: false,
            camera_extension_registered: false,
            camera_signing_ready: Self::camera_signing_ready(),
            camera_status: None,
            camera_test_busy: false,
            camera_test_status: None,
            installer_busy: false,
            update_checking: false,
            update_installing: false,
            update_install_unavailable_reason,
            update_available: None,
            update_status,
            activity,
            tray: None,
            ui: None,
            window: None,
            webview: None,
            shared_defaults,
            proxy,
        }
    }

    fn record_activity(
        &mut self,
        level: ActivityLevel,
        category: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.activity.push(level, category, message);
    }

    fn create_shell(
        &mut self,
        event_loop: &EventLoopWindowTarget<UserEvent>,
    ) -> anyhow::Result<()> {
        self.create_tray()?;
        self.create_window(event_loop)?;
        self.probe_camera();
        Ok(())
    }

    fn create_tray(&mut self) -> anyhow::Result<()> {
        let (menu, ui) = self.build_menu()?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(true)
            .with_tooltip(APP_NAME)
            .with_icon(status_icon()?)
            .with_icon_as_template(true)
            .build()?;
        self.tray = Some(tray);
        self.ui = Some(ui);
        self.refresh_ui();
        Ok(())
    }

    fn create_window(
        &mut self,
        event_loop: &EventLoopWindowTarget<UserEvent>,
    ) -> anyhow::Result<()> {
        let window = WindowBuilder::new()
            .with_title(APP_NAME)
            .with_inner_size(LogicalSize::new(900.0, 820.0))
            .with_min_inner_size(LogicalSize::new(640.0, 660.0))
            .with_visible(true)
            .build(event_loop)?;
        let proxy = self.proxy.clone();
        let webview = WebViewBuilder::new()
            .with_html(APP_HTML)
            .with_ipc_handler(move |request| {
                let _ = proxy.send_event(UserEvent::Gui(request.body().clone()));
            })
            .with_accept_first_mouse(true)
            .build(&window)?;
        self.window = Some(window);
        self.webview = Some(webview);
        self.refresh_ui();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn build_menu(&self) -> anyhow::Result<(Menu, MenuUi)> {
        let menu = Menu::new();
        let status = MenuItem::with_id("status", self.status_text(), false, None);
        let camera = CheckMenuItem::with_id(
            MENU_CAMERA,
            "Freeze camera",
            self.camera_available,
            self.state.camera_frozen,
            Some(menu_accelerator(MenuCode::KeyF)),
        );
        let microphone = CheckMenuItem::with_id(
            MENU_MICROPHONE,
            "Mute microphone",
            self.audio.is_some(),
            self.state.microphone_muted,
            Some(menu_accelerator(MenuCode::KeyM)),
        );
        let stop = MenuItem::with_id(
            MENU_STOP,
            "Stop sound clip",
            self.audio.is_some(),
            Some(menu_accelerator(MenuCode::KeyX)),
        );
        let replace_microphone = CheckMenuItem::with_id(
            MENU_REPLACE_MICROPHONE,
            "Replace microphone while a clip plays",
            self.audio.is_some(),
            self.config.replace_microphone_while_playing,
            None,
        );
        let voice_effect_menu = Submenu::new("Voice effect", self.audio.is_some());
        let mut voice_effect_items = Vec::with_capacity(VoiceEffect::ALL.len());
        for effect in VoiceEffect::ALL {
            let item = CheckMenuItem::with_id(
                format!("{MENU_VOICE_EFFECT_PREFIX}{}", effect.id()),
                effect.label(),
                true,
                self.config.voice_effect.effect == effect,
                None,
            );
            voice_effect_menu.append(&item)?;
            voice_effect_items.push((item, effect));
        }
        voice_effect_menu.append(&PredefinedMenuItem::separator())?;
        voice_effect_menu.append(&MenuItem::with_id(
            MENU_CYCLE_VOICE_EFFECT,
            "Next voice effect",
            self.audio.is_some(),
            Some(menu_accelerator(MenuCode::KeyV)),
        ))?;

        let clips_menu = Submenu::new("Sound clips", !self.clips.is_empty());
        for (index, path) in self.clips.iter().enumerate() {
            let label = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let accelerator = digit_code(index).map(menu_accelerator);
            clips_menu.append(&MenuItem::with_id(
                format!("clip.{index}"),
                label,
                self.audio.is_some(),
                accelerator,
            ))?;
        }
        if self.clips.is_empty() {
            clips_menu.append(&MenuItem::new(
                "No clips yet - open the Clips folder",
                false,
                None,
            ))?;
        }

        let device_menus = self.build_device_menus()?;
        let check_updates = MenuItem::with_id(
            MENU_CHECK_UPDATES,
            "Check for updates...",
            !self.update_checking && !self.update_installing,
            None,
        );

        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(MENU_OPEN_WINDOW, "Open Static Stream", true, None),
            &PredefinedMenuItem::separator(),
            &camera,
            &microphone,
            &replace_microphone,
            &voice_effect_menu,
            &stop,
            &PredefinedMenuItem::separator(),
            &clips_menu,
            &device_menus.input,
            &device_menus.output,
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(
                MENU_INSTALL_CAMERA,
                "Install or update virtual camera",
                Self::camera_activation_helper().is_some() && self.camera_signing_ready,
                None,
            ),
            &MenuItem::with_id(
                MENU_UNINSTALL_CAMERA,
                "Uninstall Static Camera",
                self.camera_extension_registered
                    && Self::camera_activation_helper().is_some()
                    && self.camera_signing_ready,
                None,
            ),
            &MenuItem::with_id(
                MENU_INSTALL_AUDIO,
                "Install or update virtual microphone",
                Self::audio_installer().is_some(),
                None,
            ),
            &MenuItem::with_id(
                MENU_UNINSTALL_AUDIO,
                "Uninstall Static Microphone",
                self.audio_driver_installed() && Self::audio_installer().is_some(),
                None,
            ),
            &MenuItem::with_id(MENU_OPEN_CLIPS, "Open Clips folder", true, None),
            &MenuItem::with_id(MENU_REFRESH, "Refresh devices and clips", true, None),
            &check_updates,
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(
                MENU_QUIT,
                "Quit Static Stream",
                true,
                Some(Accelerator::new(Some(MenuModifiers::SUPER), MenuCode::KeyQ)),
            ),
        ])?;

        Ok((
            menu,
            MenuUi {
                status,
                check_updates,
                camera,
                microphone,
                replace_microphone,
                voice_effect_items,
                input_items: device_menus.input_items,
                output_items: device_menus.output_items,
                selections: device_menus.selections,
            },
        ))
    }

    fn build_device_menus(&self) -> anyhow::Result<DeviceMenus> {
        let input_menu = Submenu::new("Microphone input", true);
        let output_menu = Submenu::new("Virtual microphone output", true);
        let mut input_items = Vec::new();
        let mut output_items = Vec::new();
        let mut selections = HashMap::new();
        for (index, device) in self.devices.iter().enumerate() {
            if device.is_input && !device.is_probable_loopback {
                let id = format!("input.{index}");
                let selected = self
                    .selected_input_name()
                    .is_some_and(|name| name == device.name);
                let item = CheckMenuItem::with_id(&id, &device.name, true, selected, None);
                input_menu.append(&item)?;
                input_items.push((item, device.name.clone()));
                selections.insert(id, DeviceSelection::Input(device.name.clone()));
            }
            if device.is_output && device.is_probable_loopback {
                let id = format!("output.{index}");
                let selected = self
                    .selected_output_name()
                    .is_some_and(|name| name == device.name);
                let label = if device.is_probable_loopback {
                    format!("{} (virtual)", device.name)
                } else {
                    device.name.clone()
                };
                let item = CheckMenuItem::with_id(&id, label, true, selected, None);
                output_menu.append(&item)?;
                output_items.push((item, device.name.clone()));
                selections.insert(id, DeviceSelection::Output(device.name.clone()));
            }
        }
        Ok(DeviceMenus {
            input: input_menu,
            output: output_menu,
            input_items,
            output_items,
            selections,
        })
    }

    fn handle_menu(&mut self, id: &str) -> bool {
        self.record_activity(
            ActivityLevel::Info,
            "Input",
            format!("Menu command received: {id}."),
        );
        match id {
            MENU_OPEN_WINDOW => self.show_window(),
            MENU_CAMERA => self.dispatch(Action::ToggleCameraFreeze),
            MENU_MICROPHONE => self.dispatch(Action::ToggleMicrophoneMute),
            MENU_REPLACE_MICROPHONE => self.toggle_microphone_replacement(),
            MENU_CYCLE_VOICE_EFFECT => self.cycle_voice_effect(),
            MENU_STOP => self.dispatch(Action::StopClips),
            MENU_INSTALL_CAMERA => self.run_camera_operation(DeviceOperation::Install),
            MENU_UNINSTALL_CAMERA => {
                self.request_device_removal("Static Camera", "uninstallCamera");
            }
            MENU_INSTALL_AUDIO => self.run_audio_operation(DeviceOperation::Install),
            MENU_UNINSTALL_AUDIO => {
                self.request_device_removal("Static Microphone", "uninstallAudio");
            }
            MENU_OPEN_CLIPS => self.open_clips_folder(),
            MENU_REFRESH => self.refresh_devices_and_menu(),
            MENU_CHECK_UPDATES => self.check_for_updates(Duration::ZERO),
            MENU_QUIT => return true,
            _ if id.starts_with("clip.") => {
                if let Some(path) = id
                    .strip_prefix("clip.")
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| self.clips.get(index))
                    .cloned()
                {
                    self.dispatch(Action::PlayClip(path));
                }
            }
            _ if id.starts_with(MENU_VOICE_EFFECT_PREFIX) => {
                if let Some(effect) = id
                    .strip_prefix(MENU_VOICE_EFFECT_PREFIX)
                    .and_then(VoiceEffect::from_id)
                {
                    self.set_voice_effect(effect);
                }
            }
            _ => {
                let selection = self
                    .ui
                    .as_ref()
                    .and_then(|ui| ui.selections.get(id))
                    .cloned();
                if let Some(selection) = selection {
                    self.select_device(selection);
                }
            }
        }
        false
    }

    fn handle_shortcut(&mut self, action: ShortcutAction) {
        self.record_activity(
            ActivityLevel::Info,
            "Input",
            format!("Global shortcut received: {}.", shortcut_label(action)),
        );
        match action {
            ShortcutAction::ToggleCamera => self.dispatch(Action::ToggleCameraFreeze),
            ShortcutAction::ToggleMicrophone => self.dispatch(Action::ToggleMicrophoneMute),
            ShortcutAction::CycleVoiceEffect => self.cycle_voice_effect(),
            ShortcutAction::StopClips => self.dispatch(Action::StopClips),
            ShortcutAction::PlayClip(index) => {
                if let Some(path) = self.clips.get(index).cloned() {
                    self.dispatch(Action::PlayClip(path));
                }
            }
        }
    }

    fn handle_gui(&mut self, message: &str) {
        let command = match serde_json::from_str::<GuiCommand>(message) {
            Ok(command) => command,
            Err(error) => {
                tracing::warn!("ignored invalid GUI command: {error}");
                self.record_activity(
                    ActivityLevel::Warning,
                    "Input",
                    format!("Ignored invalid window command: {error}"),
                );
                self.refresh_ui();
                return;
            }
        };
        if !matches!(&command, GuiCommand::ClearActivity) {
            self.record_activity(ActivityLevel::Info, "Input", gui_command_message(&command));
        }
        match command {
            GuiCommand::Ready => self.refresh_ui(),
            GuiCommand::SetCamera { frozen } => self.set_camera_frozen(frozen),
            GuiCommand::SetMuted { muted } => self.set_microphone_muted(muted),
            GuiCommand::SetReplace { enabled } => self.set_microphone_replacement(enabled),
            GuiCommand::SetClipGain { gain } => self.set_clip_gain(gain),
            GuiCommand::SetVoiceEffect { effect } => self.set_voice_effect(effect),
            GuiCommand::SetVoiceIntensity { intensity } => self.set_voice_intensity(intensity),
            GuiCommand::SetVoiceMix { mix } => self.set_voice_mix(mix),
            GuiCommand::SetSpeakerMonitor { enabled } => self.set_speaker_monitor(enabled),
            GuiCommand::SetSpeakerGain { gain } => self.set_speaker_gain(gain),
            GuiCommand::SelectInput { name } => {
                self.select_device(DeviceSelection::Input(name));
            }
            GuiCommand::SelectOutput { name } => {
                self.select_device(DeviceSelection::Output(name));
            }
            GuiCommand::PlayClip { index } => {
                if let Some(path) = self.clips.get(index).cloned() {
                    self.dispatch(Action::PlayClip(path));
                }
            }
            GuiCommand::StopClip => self.dispatch(Action::StopClips),
            GuiCommand::OpenClips => self.open_clips_folder(),
            GuiCommand::RefreshClips => self.refresh_clips(),
            GuiCommand::Refresh => self.refresh_devices_and_menu(),
            GuiCommand::TestCamera => self.run_camera_development_test(),
            GuiCommand::InstallCamera => self.run_camera_operation(DeviceOperation::Install),
            GuiCommand::InstallAudio => self.run_audio_operation(DeviceOperation::Install),
            GuiCommand::UninstallCamera => self.run_camera_operation(DeviceOperation::Uninstall),
            GuiCommand::UninstallAudio => self.run_audio_operation(DeviceOperation::Uninstall),
            GuiCommand::SetAutoCheckUpdates { enabled } => {
                self.set_auto_check_updates(enabled);
            }
            GuiCommand::CheckForUpdates => self.check_for_updates(Duration::ZERO),
            GuiCommand::InstallUpdate => self.install_update(),
            GuiCommand::ClearActivity => {
                self.activity.clear();
                self.refresh_ui();
            }
        }
    }

    fn set_auto_check_updates(&mut self, enabled: bool) {
        if self.config.auto_check_updates == enabled {
            return;
        }
        self.config.auto_check_updates = enabled;
        self.record_activity(
            ActivityLevel::Info,
            "Updates",
            if enabled {
                "Automatic update checks enabled."
            } else {
                "Automatic update checks disabled."
            },
        );
        if let Err(error) = self.paths.save(&self.config) {
            self.update_status = format!("Could not save the update setting: {error}");
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save the update setting: {error}"),
            );
        }
        if enabled {
            self.check_for_updates(Duration::ZERO);
        } else {
            self.refresh_ui();
        }
    }

    fn check_for_updates(&mut self, delay: Duration) {
        if self.update_checking || self.update_installing {
            return;
        }
        self.update_checking = true;
        self.update_status = if delay.is_zero() {
            "Checking GitHub for updates...".into()
        } else {
            "Automatic update check scheduled...".into()
        };
        self.record_activity(
            ActivityLevel::Info,
            "Updates",
            if delay.is_zero() {
                "Checking GitHub for a newer Static Stream release."
            } else {
                "Scheduled the automatic update check after app startup."
            },
        );
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let result = updates::check_for_update(env!("CARGO_PKG_VERSION"))
                .map_err(|error| error.to_string());
            let _ = proxy.send_event(UserEvent::UpdateCheckFinished(result));
        });
        self.refresh_ui();
    }

    fn handle_update_check(&mut self, result: Result<Option<UpdateManifest>, String>) {
        self.update_checking = false;
        match result {
            Ok(Some(manifest)) => {
                self.update_status = self.update_install_unavailable_reason.as_ref().map_or_else(
                    || {
                        format!(
                            "Version {} is available. Review it, then install when convenient.",
                            manifest.version
                        )
                    },
                    |reason| {
                        format!(
                            "Version {} is available, but automatic installation is \
                                 unavailable: {reason}.",
                            manifest.version
                        )
                    },
                );
                self.record_activity(
                    ActivityLevel::Info,
                    "Updates",
                    format!("Static Stream {} is available.", manifest.version),
                );
                self.update_available = Some(manifest);
            }
            Ok(None) => {
                self.update_available = None;
                self.update_status =
                    format!("Static Stream {} is up to date.", env!("CARGO_PKG_VERSION"));
                self.record_activity(
                    ActivityLevel::Info,
                    "Updates",
                    "No newer Static Stream release is available.",
                );
            }
            Err(error) => {
                self.update_status = format!("Could not check for updates: {error}");
                self.record_activity(
                    ActivityLevel::Warning,
                    "Updates",
                    format!("Update check failed: {error}"),
                );
            }
        }
        self.refresh_ui();
    }

    fn install_update(&mut self) {
        if self.update_checking || self.update_installing {
            return;
        }
        if let Some(reason) = &self.update_install_unavailable_reason {
            self.update_status = format!("Automatic installation is unavailable: {reason}.");
            self.record_activity(
                ActivityLevel::Warning,
                "Updates",
                format!("Update installation is unavailable: {reason}"),
            );
            self.refresh_ui();
            return;
        }
        let Some(manifest) = self.update_available.clone() else {
            self.update_status = "Check for updates before installing.".into();
            self.refresh_ui();
            return;
        };

        self.update_installing = true;
        self.update_status = format!(
            "Downloading and verifying Static Stream {}...",
            manifest.version
        );
        self.record_activity(
            ActivityLevel::Info,
            "Updates",
            format!(
                "Downloading and verifying Static Stream {}.",
                manifest.version
            ),
        );
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = updates::stage_update(&manifest).map_err(|error| error.to_string());
            let _ = proxy.send_event(UserEvent::UpdateStaged(result));
        });
        self.refresh_ui();
    }

    fn handle_staged_update(&mut self, result: Result<StagedUpdate, String>) -> bool {
        match result {
            Ok(update) => match updates::launch_installer(&update) {
                Ok(()) => {
                    self.update_status = "Installing the update and restarting...".into();
                    self.record_activity(
                        ActivityLevel::Info,
                        "Updates",
                        "Verified update staged. Static Stream will restart.",
                    );
                    self.refresh_ui();
                    true
                }
                Err(error) => {
                    self.update_installing = false;
                    self.update_status = format!("Could not start the update installer: {error}");
                    self.record_activity(
                        ActivityLevel::Error,
                        "Updates",
                        format!("Could not start the update installer: {error}"),
                    );
                    self.refresh_ui();
                    false
                }
            },
            Err(error) => {
                self.update_installing = false;
                self.update_status = format!("Could not install the update: {error}");
                self.record_activity(
                    ActivityLevel::Error,
                    "Updates",
                    format!("Update staging failed: {error}"),
                );
                self.refresh_ui();
                false
            }
        }
    }

    fn set_camera_frozen(&mut self, frozen: bool) {
        if self.state.camera_frozen != frozen {
            self.dispatch(Action::ToggleCameraFreeze);
        }
    }

    const fn reset_clip_timing(&mut self) {
        self.clip_started_at = None;
        self.clip_duration = None;
    }

    fn set_microphone_muted(&mut self, muted: bool) {
        if self.state.microphone_muted != muted {
            self.dispatch(Action::ToggleMicrophoneMute);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&mut self, action: Action) {
        if matches!(action, Action::ToggleCameraFreeze) && !self.camera_available {
            self.camera_status =
                Some("Static Camera is unavailable; install and approve it first.".into());
            self.record_activity(
                ActivityLevel::Warning,
                "Camera",
                "Freeze request blocked because Static Camera is unavailable.",
            );
            self.refresh_ui();
            return;
        }
        if matches!(
            action,
            Action::ToggleMicrophoneMute | Action::PlayClip(_) | Action::StopClips
        ) && self.audio.is_none()
        {
            self.audio_error
                .get_or_insert_with(|| "Static Microphone is not ready.".into());
            if let Action::PlayClip(path) = &action {
                self.clip_phase = ClipPhase::Error;
                self.clip_name = Some(clip_display_name(path));
                self.clip_message =
                    "Static Microphone is not ready. Install it before playing clips.".into();
            }
            self.record_activity(
                ActivityLevel::Warning,
                "Audio",
                "Audio command blocked because routing is not ready.",
            );
            self.refresh_ui();
            return;
        }

        let effect = self.state.apply(action);
        match effect {
            Effect::SetCameraFrozen(frozen) => {
                if let Some(defaults) = &self.shared_defaults {
                    defaults.setBool_forKey(frozen, &NSString::from_str(CAMERA_FROZEN_KEY));
                    let _ = defaults.synchronize();
                }
                self.record_activity(
                    ActivityLevel::Info,
                    "Camera",
                    if frozen {
                        "Freeze enabled; the camera extension will hold its latest frame."
                    } else {
                        "Freeze disabled; live camera pass-through requested."
                    },
                );
            }
            Effect::SetMicrophoneMuted(muted) => {
                let _ = self.send_audio(AudioCommand::SetMuted(muted));
                self.record_activity(
                    ActivityLevel::Info,
                    "Audio",
                    if muted {
                        "Microphone mute enabled."
                    } else {
                        "Microphone mute disabled."
                    },
                );
            }
            Effect::PlayClip(path) => {
                let name = clip_display_name(&path);
                let request_id = self.next_clip_request_id;
                self.next_clip_request_id = self.next_clip_request_id.wrapping_add(1).max(1);
                self.active_clip_request = Some(request_id);
                self.clip_phase = ClipPhase::Loading;
                self.clip_name = Some(name.clone());
                self.clip_message = format!("Loading {name}...");
                self.reset_clip_timing();
                self.record_activity(
                    ActivityLevel::Info,
                    "Clips",
                    format!("Decoding sound clip: {name}."),
                );
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let decoded = clips::decode(&path).map_err(|error| error.to_string());
                    let _ = proxy.send_event(UserEvent::ClipDecoded {
                        request_id,
                        result: decoded,
                    });
                });
            }
            Effect::StopClips => {
                self.active_clip_request = None;
                self.clip_phase = ClipPhase::Stopped;
                self.reset_clip_timing();
                self.clip_message = self.clip_name.as_ref().map_or_else(
                    || "No sound clip is playing.".into(),
                    |name| format!("Stopped {name}."),
                );
                let _ = self.send_audio(AudioCommand::Stop);
                self.record_activity(
                    ActivityLevel::Info,
                    "Clips",
                    "Stopped or cancelled sound clip playback.",
                );
            }
        }
        self.refresh_ui();
    }

    fn handle_decoded_clip(&mut self, request_id: u64, result: Result<DecodedClip, String>) {
        if self.active_clip_request != Some(request_id) {
            self.record_activity(
                ActivityLevel::Info,
                "Clips",
                format!("Discarded stale decoded clip request {request_id}."),
            );
            self.refresh_ui();
            return;
        }

        match result {
            Ok(clip) => {
                let message = format!(
                    "Decoded {} ({:.2}s, {} Hz, {} channel(s)); playback queued.",
                    clip.name,
                    clip.duration_seconds(),
                    clip.sample_rate,
                    clip.channels
                );
                self.audio_error = None;
                if self.send_audio(AudioCommand::Play { request_id, clip }) {
                    self.clip_phase = ClipPhase::Starting;
                    self.clip_message = self.clip_name.as_ref().map_or_else(
                        || "Starting sound clip...".into(),
                        |name| format!("Starting {name}..."),
                    );
                    self.record_activity(ActivityLevel::Info, "Clips", message);
                } else {
                    self.active_clip_request = None;
                    self.state.active_clip = None;
                    self.clip_phase = ClipPhase::Error;
                    self.reset_clip_timing();
                    self.clip_message = self
                        .audio_error
                        .clone()
                        .unwrap_or_else(|| "Could not start sound clip playback.".into());
                }
            }
            Err(error) => {
                self.active_clip_request = None;
                self.state.active_clip = None;
                self.clip_phase = ClipPhase::Error;
                self.clip_message = format!("Could not load clip: {error}");
                self.reset_clip_timing();
                self.audio_error = Some(format!("Clip error: {error}"));
                tracing::warn!("{error}");
                self.record_activity(
                    ActivityLevel::Error,
                    "Clips",
                    format!("Sound clip decoding failed: {error}"),
                );
            }
        }
        self.refresh_ui();
    }

    fn send_audio(&mut self, command: AudioCommand) -> bool {
        let command_name = audio_command_name(&command);
        let Some(audio) = &self.audio else {
            self.audio_error
                .get_or_insert_with(|| "Audio setup required".into());
            self.record_activity(
                ActivityLevel::Warning,
                "Audio",
                format!("{command_name} could not be sent because audio is unavailable."),
            );
            return false;
        };
        if let Err(error) = audio.command_sender().try_send(command) {
            self.audio_error = Some(format!("Audio command failed: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Audio",
                format!("{command_name} could not be queued: {error}"),
            );
            return false;
        }
        true
    }

    fn poll_audio_telemetry(&mut self) {
        if let Some((request_id, deadline)) = self.audio_startup_warning {
            if Instant::now() >= deadline {
                self.audio_startup_warning = None;
                self.handle_audio_startup_slow(request_id);
            }
        }
        let levels = self
            .audio
            .as_ref()
            .map_or_else(AudioLevels::default, AudioEngine::take_levels);
        let levels_changed = levels != self.audio_levels;
        if levels_changed {
            self.audio_levels = levels;
        }
        if self.window_visible() && (levels_changed || self.clip_phase == ClipPhase::Playing) {
            self.refresh_telemetry();
        }
    }

    fn telemetry_interval(&self) -> Duration {
        telemetry_interval(self.window_visible(), self.audio.is_some())
    }

    fn window_visible(&self) -> bool {
        self.window.as_ref().is_some_and(Window::is_visible)
    }

    fn handle_audio_status(&mut self, status: AudioEngineStatus) {
        match status {
            AudioEngineStatus::Ready { .. } => {}
            AudioEngineStatus::ClipStarted {
                request_id,
                name,
                duration_ms,
            } => {
                self.record_activity(
                    ActivityLevel::Info,
                    "Clips",
                    format!(
                        "Playback started: {name} ({:.2}s).",
                        duration_ms as f64 / 1_000.0
                    ),
                );
                if self.active_clip_request == Some(request_id) {
                    self.clip_phase = ClipPhase::Playing;
                    self.clip_name = Some(name.to_string());
                    self.clip_started_at = Some(Instant::now());
                    self.clip_duration = Some(Duration::from_millis(duration_ms));
                    let output = self
                        .audio
                        .as_ref()
                        .and_then(|audio| match audio.ready_status() {
                            AudioEngineStatus::Ready { output, .. } => Some(output.as_str()),
                            _ => None,
                        })
                        .unwrap_or("virtual microphone");
                    self.clip_message = format!("Playing {name} through {output}.");
                }
            }
            AudioEngineStatus::ClipFinished { request_id, name } => {
                self.record_activity(
                    ActivityLevel::Info,
                    "Clips",
                    format!("Playback finished: {name}."),
                );
                if self.active_clip_request == Some(request_id) {
                    self.active_clip_request = None;
                    self.state.active_clip = None;
                    self.clip_phase = ClipPhase::Finished;
                    self.clip_name = Some(name.to_string());
                    self.clip_message = format!("Finished {name}.");
                }
            }
            AudioEngineStatus::ClipStopped { request_id, name } => {
                let label = name.as_deref().unwrap_or("sound clip").to_owned();
                self.record_activity(
                    ActivityLevel::Info,
                    "Clips",
                    format!("Playback stopped: {label}."),
                );
                if request_id.is_some() && request_id == self.active_clip_request {
                    self.active_clip_request = None;
                    self.state.active_clip = None;
                    self.clip_phase = ClipPhase::Stopped;
                    self.clip_name = name.map(|name| name.to_string());
                    self.clip_message = format!("Stopped {label}.");
                    self.reset_clip_timing();
                }
            }
            AudioEngineStatus::VoiceEffectChanged(effect) => {
                self.record_activity(
                    ActivityLevel::Info,
                    "Voice",
                    format!(
                        "Audio engine activated the {} voice effect.",
                        effect.label()
                    ),
                );
            }
            AudioEngineStatus::SpeakerMonitorError(error) => {
                self.speaker_monitor_error = Some(error.clone());
                self.record_activity(
                    ActivityLevel::Warning,
                    "Audio",
                    format!("Speaker monitoring failed: {error}"),
                );
            }
            AudioEngineStatus::StreamError(error) => {
                self.active_clip_request = None;
                self.state.active_clip = None;
                self.clip_phase = ClipPhase::Error;
                self.clip_message = format!("Audio stream failed: {error}");
                self.reset_clip_timing();
                self.audio_error = Some(format!("Audio stream error: {error}"));
                self.record_activity(
                    ActivityLevel::Error,
                    "Audio",
                    format!("Audio stream failed: {error}"),
                );
            }
        }
    }

    fn select_device(&mut self, selection: DeviceSelection) {
        let selection_message = match &selection {
            DeviceSelection::Input(name) => format!("Selected physical microphone: {name}."),
            DeviceSelection::Output(name) => {
                format!("Selected virtual microphone output: {name}.")
            }
        };
        self.record_activity(ActivityLevel::Info, "Devices", selection_message);
        match selection {
            DeviceSelection::Input(name) => self.config.input_device = Some(name),
            DeviceSelection::Output(name) => self.config.output_device = Some(name),
        }
        if let Err(error) = self.paths.save(&self.config) {
            self.audio_error = Some(format!("Could not save settings: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save device selection: {error}"),
            );
        }
        self.restart_audio();
        self.refresh_ui();
    }

    fn restart_audio(&mut self) {
        if self.active_clip_request.take().is_some() {
            self.state.active_clip = None;
            self.clip_phase = ClipPhase::Stopped;
            self.clip_message = "Playback stopped because audio routing changed.".into();
        }
        self.reset_clip_timing();
        self.current_audio_request = None;
        self.audio = None;
        self.audio_levels = AudioLevels::default();
        self.speaker_monitor_error = None;
        self.start_audio_async();
    }

    fn start_audio_async(&mut self) {
        let request_id = self.next_audio_request_id;
        self.next_audio_request_id = self.next_audio_request_id.wrapping_add(1).max(1);
        self.active_audio_request = Some(request_id);
        self.audio_startup_warning =
            Some((request_id, Instant::now() + AUDIO_STARTUP_WARNING_DELAY));
        self.audio_error = Some("Starting audio routing...".into());
        self.record_activity(
            ActivityLevel::Info,
            "Audio",
            format!("Starting audio routing request {request_id}."),
        );
        let config = self.config.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let progress_proxy = proxy.clone();
            let result = AudioEngine::start_with_progress(&config, |message| {
                let _ = progress_proxy.send_event(UserEvent::AudioProgress {
                    request_id,
                    message,
                });
            })
            .map_err(|error| error.to_string());
            let _ = proxy.send_event(UserEvent::AudioStarted { request_id, result });
        });
        self.refresh_ui();
    }

    fn handle_audio_startup_slow(&mut self, request_id: u64) {
        if self.active_audio_request != Some(request_id) {
            return;
        }
        let message = "Audio startup is taking longer than expected. Install/update Static \
                       Microphone to replace an older driver, then refresh devices."
            .to_owned();
        self.audio_error = Some(message.clone());
        self.record_activity(ActivityLevel::Warning, "Audio", message);
        self.refresh_ui();
    }

    fn handle_audio_progress(&mut self, request_id: u64, message: String) {
        if self.active_audio_request != Some(request_id) {
            return;
        }
        self.audio_error = Some(message.clone());
        self.record_activity(ActivityLevel::Info, "Audio", message);
        self.refresh_ui();
    }

    fn handle_audio_started(&mut self, request_id: u64, result: Result<AudioEngine, String>) {
        if self.active_audio_request != Some(request_id) {
            self.record_activity(
                ActivityLevel::Info,
                "Audio",
                format!("Discarded stale audio routing request {request_id}."),
            );
            return;
        }
        self.audio_startup_warning = None;
        self.active_audio_request = None;
        match result {
            Ok(mut engine) => {
                let ready_message = audio_ready_message(engine.ready_status());
                let _ = engine
                    .command_sender()
                    .try_send(AudioCommand::SetMuted(self.state.microphone_muted));
                let statuses = engine
                    .take_status_receiver()
                    .expect("new audio engines always own a status receiver");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    while let Ok(status) = statuses.recv() {
                        if proxy
                            .send_event(UserEvent::AudioStatus { request_id, status })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                self.current_audio_request = Some(request_id);
                self.audio = Some(engine);
                self.audio_error = None;
                self.record_activity(ActivityLevel::Info, "Audio", ready_message);
            }
            Err(error) => {
                self.current_audio_request = None;
                tracing::warn!("audio is not ready: {error}");
                self.audio_error = Some(error.clone());
                self.record_activity(
                    ActivityLevel::Warning,
                    "Audio",
                    format!("Audio routing did not start: {error}"),
                );
            }
        }
        self.rebuild_menu();
        self.refresh_ui();
    }

    fn handle_audio_status_event(&mut self, request_id: u64, status: AudioEngineStatus) {
        if self.current_audio_request != Some(request_id) {
            return;
        }
        self.handle_audio_status(status);
        self.refresh_ui();
    }

    fn toggle_microphone_replacement(&mut self) {
        self.set_microphone_replacement(!self.config.replace_microphone_while_playing);
    }

    fn set_microphone_replacement(&mut self, enabled: bool) {
        if self.config.replace_microphone_while_playing == enabled {
            return;
        }
        self.config.replace_microphone_while_playing = enabled;
        self.record_activity(
            ActivityLevel::Info,
            "Audio",
            if enabled {
                "Clip mode changed to replace the microphone."
            } else {
                "Clip mode changed to mix with the microphone."
            },
        );
        if let Err(error) = self.paths.save(&self.config) {
            self.audio_error = Some(format!("Could not save settings: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save clip mode: {error}"),
            );
        }
        self.restart_audio();
        self.refresh_ui();
    }

    fn set_clip_gain(&mut self, gain: f32) {
        let gain = gain.clamp(0.0, 2.0);
        if (self.config.clip_gain - gain).abs() < f32::EPSILON {
            return;
        }
        self.config.clip_gain = gain;
        let _ = self.send_audio(AudioCommand::SetClipGain(gain));
        self.record_activity(
            ActivityLevel::Info,
            "Audio",
            format!(
                "Virtual microphone clip volume set to {:.0}%.",
                gain * 100.0
            ),
        );
        if let Err(error) = self.paths.save(&self.config) {
            self.audio_error = Some(format!("Could not save settings: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save clip volume: {error}"),
            );
        }
        self.refresh_ui();
    }

    fn set_voice_effect(&mut self, effect: VoiceEffect) {
        let settings = VoiceEffectSettings {
            effect,
            ..self.config.voice_effect
        };
        self.update_voice_effect(
            settings,
            format!("Voice effect changed to {}.", effect.label()),
        );
    }

    fn cycle_voice_effect(&mut self) {
        let current = VoiceEffect::ALL
            .iter()
            .position(|effect| *effect == self.config.voice_effect.effect)
            .unwrap_or_default();
        let next = VoiceEffect::ALL[(current + 1) % VoiceEffect::ALL.len()];
        self.set_voice_effect(next);
    }

    fn set_voice_intensity(&mut self, intensity: f32) {
        let settings = VoiceEffectSettings {
            intensity,
            ..self.config.voice_effect
        }
        .normalized();
        self.update_voice_effect(
            settings,
            format!(
                "Voice effect intensity set to {:.0}%.",
                settings.intensity * 100.0
            ),
        );
    }

    fn set_voice_mix(&mut self, mix: f32) {
        let settings = VoiceEffectSettings {
            mix,
            ..self.config.voice_effect
        }
        .normalized();
        self.update_voice_effect(
            settings,
            format!("Voice effect mix set to {:.0}%.", settings.mix * 100.0),
        );
    }

    fn update_voice_effect(&mut self, settings: VoiceEffectSettings, message: String) {
        let settings = settings.normalized();
        if self.config.voice_effect == settings {
            return;
        }
        self.config.voice_effect = settings;
        let _ = self.send_audio(AudioCommand::SetVoiceEffect(settings));
        self.record_activity(ActivityLevel::Info, "Voice", message);
        if let Err(error) = self.paths.save(&self.config) {
            self.audio_error = Some(format!("Could not save settings: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save voice effect settings: {error}"),
            );
        }
        self.refresh_ui();
    }

    fn set_speaker_monitor(&mut self, enabled: bool) {
        if self.config.play_clips_on_speakers == enabled {
            return;
        }
        self.config.play_clips_on_speakers = enabled;
        self.record_activity(
            ActivityLevel::Info,
            "Audio",
            if enabled {
                "Clip speaker monitoring enabled."
            } else {
                "Clip speaker monitoring disabled."
            },
        );
        if let Err(error) = self.paths.save(&self.config) {
            self.audio_error = Some(format!("Could not save settings: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save speaker monitoring setting: {error}"),
            );
        }
        self.restart_audio();
        self.refresh_ui();
    }

    fn set_speaker_gain(&mut self, gain: f32) {
        let gain = gain.clamp(0.0, 2.0);
        if (self.config.speaker_gain - gain).abs() < f32::EPSILON {
            return;
        }
        self.config.speaker_gain = gain;
        let _ = self.send_audio(AudioCommand::SetSpeakerGain(gain));
        self.record_activity(
            ActivityLevel::Info,
            "Audio",
            format!("Speaker clip volume set to {:.0}%.", gain * 100.0),
        );
        if let Err(error) = self.paths.save(&self.config) {
            self.audio_error = Some(format!("Could not save settings: {error}"));
            self.record_activity(
                ActivityLevel::Error,
                "Config",
                format!("Could not save speaker volume: {error}"),
            );
        }
        self.refresh_ui();
    }

    fn refresh_clips(&mut self) {
        self.clips = clips::discover(&self.paths.clips_dir);
        self.record_activity(
            ActivityLevel::Info,
            "Clips",
            format!("Clip refresh found {} sound clip(s).", self.clips.len()),
        );
        self.rebuild_menu();
        self.refresh_ui();
    }

    fn refresh_devices_and_menu(&mut self) {
        self.clips = clips::discover(&self.paths.clips_dir);
        self.devices = match list_devices() {
            Ok(devices) => devices,
            Err(error) => {
                self.audio_error = Some(format!("Could not list devices: {error}"));
                self.record_activity(
                    ActivityLevel::Error,
                    "Devices",
                    format!("Audio device discovery failed: {error}"),
                );
                Vec::new()
            }
        };
        self.record_activity(
            ActivityLevel::Info,
            "Devices",
            format!(
                "Refresh found {} audio devices and {} sound clips.",
                self.devices.len(),
                self.clips.len()
            ),
        );
        self.restart_audio();
        self.probe_camera();
        match self.build_menu() {
            Ok((menu, ui)) => {
                if let Some(tray) = &self.tray {
                    tray.set_menu(Some(Box::new(menu)));
                }
                self.ui = Some(ui);
                self.refresh_ui();
            }
            Err(error) => {
                self.audio_error = Some(format!("Could not rebuild menu: {error}"));
                self.record_activity(
                    ActivityLevel::Error,
                    "App",
                    format!("Menu rebuild failed: {error}"),
                );
                self.refresh_ui();
            }
        }
    }

    fn open_clips_folder(&mut self) {
        match Command::new("open").arg(&self.paths.clips_dir).spawn() {
            Ok(_) => self.record_activity(
                ActivityLevel::Info,
                "Clips",
                "Opened the Clips folder in Finder.",
            ),
            Err(error) => {
                self.audio_error = Some(format!("Could not open Clips folder: {error}"));
                self.record_activity(
                    ActivityLevel::Error,
                    "Clips",
                    format!("Could not open the Clips folder: {error}"),
                );
            }
        }
        self.refresh_ui();
    }

    fn run_camera_development_test(&mut self) {
        if self.camera_test_busy {
            return;
        }
        let Some(helper) = Self::camera_development_test_helper() else {
            self.camera_test_status =
                Some("Build the macOS app bundle before running the camera test.".into());
            self.record_activity(
                ActivityLevel::Error,
                "Camera",
                "Unsigned camera test helper is unavailable.",
            );
            self.refresh_ui();
            return;
        };
        self.camera_test_busy = true;
        self.camera_test_status =
            Some("Testing live, frozen, and resumed frame selection...".into());
        self.record_activity(
            ActivityLevel::Info,
            "Camera",
            "Starting the unsigned camera freeze test.",
        );
        self.refresh_ui();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = Command::new(helper)
                .output()
                .map_err(|error| format!("Could not start the camera test: {error}"))
                .and_then(|output| {
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                    if output.status.success() {
                        Ok(stdout)
                    } else if stderr.is_empty() {
                        Err(format!("Camera test exited with {}", output.status))
                    } else {
                        Err(stderr)
                    }
                });
            let _ = proxy.send_event(UserEvent::CameraDevelopmentTestFinished(result));
        });
    }

    fn handle_camera_development_test(&mut self, result: Result<String, String>) {
        self.camera_test_busy = false;
        match result {
            Ok(message) => {
                self.camera_test_status = Some(message.clone());
                self.record_activity(ActivityLevel::Info, "Camera", message);
            }
            Err(error) => {
                self.camera_test_status = Some(error.clone());
                self.record_activity(
                    ActivityLevel::Error,
                    "Camera",
                    format!("Unsigned camera test failed: {error}"),
                );
            }
        }
        self.refresh_ui();
    }

    fn camera_development_test_helper() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let helper = executable.parent()?.join("static-stream-camera-test");
        helper.is_file().then_some(helper)
    }

    fn run_camera_operation(&mut self, operation: DeviceOperation) {
        if self.installer_busy {
            self.record_activity(
                ActivityLevel::Warning,
                "Installer",
                "Camera operation ignored because another device operation is active.",
            );
            self.refresh_ui();
            return;
        }
        let Some(helper) = Self::camera_activation_helper() else {
            self.camera_status =
                Some("Build the macOS app bundle before installing the camera.".into());
            self.record_activity(
                ActivityLevel::Error,
                "Installer",
                "Camera helper is unavailable in the current executable.",
            );
            self.refresh_ui();
            return;
        };
        if !Self::is_running_from_applications() {
            self.camera_status = Some(
                "Move Static Stream.app to /Applications before installing its camera.".into(),
            );
            self.record_activity(
                ActivityLevel::Warning,
                "Installer",
                "Camera operation blocked because the app is outside /Applications.",
            );
            self.refresh_ui();
            return;
        }
        if !self.camera_signing_ready {
            self.camera_status =
                Some("This build is not signed by an Apple development team.".into());
            self.record_activity(
                ActivityLevel::Error,
                "Installer",
                "Camera operation blocked because the app has no Apple team signature.",
            );
            self.refresh_ui();
            return;
        }
        self.installer_busy = true;
        self.camera_status = Some(match operation {
            DeviceOperation::Install => "Waiting for macOS camera approval...".into(),
            DeviceOperation::Uninstall => "Removing Static Camera...".into(),
        });
        self.record_activity(
            ActivityLevel::Info,
            "Installer",
            format!("Starting Static Camera {}.", operation.label()),
        );
        self.refresh_ui();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = Command::new(helper)
                .arg(operation.command())
                .output()
                .map_err(|error| format!("Could not start the camera operation: {error}"))
                .and_then(|output| {
                    if output.status.success() {
                        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
                    } else {
                        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                        Err(if detail.is_empty() {
                            format!("Camera operation exited with {}", output.status)
                        } else {
                            detail
                        })
                    }
                });
            let _ = proxy.send_event(UserEvent::CameraOperationFinished(operation, result));
        });
    }

    fn camera_activation_helper() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let helper = executable.parent()?.join("static-stream-activate");
        helper.is_file().then_some(helper)
    }

    fn run_audio_operation(&mut self, operation: DeviceOperation) {
        if self.installer_busy {
            self.record_activity(
                ActivityLevel::Warning,
                "Installer",
                "Microphone operation ignored because another device operation is active.",
            );
            self.refresh_ui();
            return;
        }
        let Some(helper) = Self::audio_installer() else {
            self.audio_error =
                Some("Build the macOS app bundle before installing the microphone.".into());
            self.record_activity(
                ActivityLevel::Error,
                "Installer",
                "Audio driver helper is unavailable in the current executable.",
            );
            self.refresh_ui();
            return;
        };
        if operation == DeviceOperation::Uninstall {
            self.audio = None;
            self.active_audio_request = None;
            self.active_clip_request = None;
            self.state.active_clip = None;
            self.clip_phase = ClipPhase::Stopped;
            self.clip_message = "Playback stopped while Static Microphone is removed.".into();
            self.reset_clip_timing();
        }
        self.installer_busy = true;
        self.audio_error = Some(match operation {
            DeviceOperation::Install => "Waiting for administrator approval...".into(),
            DeviceOperation::Uninstall => {
                "Waiting for approval to remove Static Microphone...".into()
            }
        });
        self.record_activity(
            ActivityLevel::Info,
            "Installer",
            format!("Starting Static Microphone {}.", operation.label()),
        );
        self.refresh_ui();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = Command::new(helper)
                .arg(operation.command())
                .output()
                .map_err(|error| format!("Could not start the audio device operation: {error}"))
                .and_then(|output| {
                    if output.status.success() {
                        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
                    } else {
                        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                        Err(if detail.is_empty() {
                            format!("Audio device operation exited with {}", output.status)
                        } else {
                            detail
                        })
                    }
                });
            if result.is_ok() {
                std::thread::sleep(Duration::from_millis(750));
            }
            let _ = proxy.send_event(UserEvent::AudioOperationFinished(operation, result));
        });
    }

    fn audio_installer() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let helper = executable.parent()?.join("static-stream-audio-install");
        helper.is_file().then_some(helper)
    }

    fn camera_probe_helper() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        let helper = executable.parent()?.join("static-stream-probe");
        helper.is_file().then_some(helper)
    }

    fn probe_camera(&mut self) {
        let Some(helper) = Self::camera_probe_helper() else {
            self.record_activity(
                ActivityLevel::Warning,
                "Camera",
                "Camera probe helper is unavailable.",
            );
            self.refresh_ui();
            return;
        };
        self.record_activity(
            ActivityLevel::Info,
            "Camera",
            "Inspecting AVFoundation and system-extension camera state.",
        );
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = Command::new(helper)
                .output()
                .map_err(|error| format!("Could not inspect cameras: {error}"))
                .and_then(|output| {
                    if !output.status.success() {
                        return Err(format!("Camera probe exited with {}", output.status));
                    }
                    let mut probe = serde_json::from_slice::<CameraProbe>(&output.stdout)
                        .map_err(|error| format!("Invalid camera probe response: {error}"))?;
                    probe.extension_registered = camera_extension_registered();
                    Ok(probe)
                });
            let _ = proxy.send_event(UserEvent::CameraProbe(result));
        });
    }

    fn handle_camera_probe(&mut self, result: Result<CameraProbe, String>) {
        match result {
            Ok(probe) => {
                self.camera_available = probe.static_stream_camera_available;
                self.camera_extension_registered =
                    probe.extension_registered || self.camera_available;
                self.record_activity(
                    ActivityLevel::Info,
                    "Camera",
                    format!(
                        "Probe completed: available={}, extension registered={}.",
                        self.camera_available, self.camera_extension_registered
                    ),
                );
                if self.camera_available {
                    self.camera_status = None;
                } else if self.state.camera_frozen {
                    self.state.camera_frozen = false;
                    if let Some(defaults) = &self.shared_defaults {
                        defaults.setBool_forKey(false, &NSString::from_str(CAMERA_FROZEN_KEY));
                    }
                }
            }
            Err(error) => {
                self.camera_status = Some(error.clone());
                self.record_activity(
                    ActivityLevel::Error,
                    "Camera",
                    format!("Camera probe failed: {error}"),
                );
            }
        }
        self.rebuild_menu();
        self.refresh_ui();
    }

    fn handle_camera_operation(
        &mut self,
        operation: DeviceOperation,
        result: Result<String, String>,
    ) {
        self.installer_busy = false;
        match &result {
            Ok(message) => self.record_activity(
                ActivityLevel::Info,
                "Installer",
                if message.is_empty() {
                    format!("Static Camera {} completed.", operation.label())
                } else {
                    message.clone()
                },
            ),
            Err(error) => self.record_activity(
                ActivityLevel::Error,
                "Installer",
                format!("Static Camera {} failed: {error}", operation.label()),
            ),
        }
        if operation == DeviceOperation::Uninstall && result.is_ok() {
            self.camera_available = false;
            self.camera_extension_registered = false;
            self.state.camera_frozen = false;
            if let Some(defaults) = &self.shared_defaults {
                defaults.setBool_forKey(false, &NSString::from_str(CAMERA_FROZEN_KEY));
                let _ = defaults.synchronize();
            }
        }
        self.camera_status = Some(match result {
            Ok(message) if message.is_empty() => match operation {
                DeviceOperation::Install => "Camera installation completed.".into(),
                DeviceOperation::Uninstall => "Static Camera removed.".into(),
            },
            Ok(message) => message,
            Err(error) => error,
        });
        self.probe_camera();
        self.refresh_ui();
    }

    fn handle_audio_operation(
        &mut self,
        operation: DeviceOperation,
        result: Result<String, String>,
    ) {
        self.installer_busy = false;
        match &result {
            Ok(message) => self.record_activity(
                ActivityLevel::Info,
                "Installer",
                if message.is_empty() {
                    format!("Static Microphone {} completed.", operation.label())
                } else {
                    message.clone()
                },
            ),
            Err(error) => self.record_activity(
                ActivityLevel::Error,
                "Installer",
                format!("Static Microphone {} failed: {error}", operation.label()),
            ),
        }
        match result {
            Ok(message) => {
                self.audio_error = if message.is_empty() {
                    None
                } else {
                    Some(message)
                };
                self.devices = match list_devices() {
                    Ok(devices) => devices,
                    Err(error) => {
                        self.audio_error = Some(format!("Could not list devices: {error}"));
                        self.record_activity(
                            ActivityLevel::Error,
                            "Devices",
                            format!("Post-install device discovery failed: {error}"),
                        );
                        Vec::new()
                    }
                };
                self.record_activity(
                    ActivityLevel::Info,
                    "Devices",
                    format!(
                        "Post-install refresh found {} audio devices.",
                        self.devices.len()
                    ),
                );
                match operation {
                    DeviceOperation::Install if self.audio_driver_available() => {
                        self.config.output_device = Some(STATIC_MICROPHONE.into());
                    }
                    DeviceOperation::Uninstall
                        if self
                            .config
                            .output_device
                            .as_deref()
                            .is_some_and(is_static_audio_device_name) =>
                    {
                        self.config.output_device = None;
                    }
                    _ => {}
                }
                if let Err(error) = self.paths.save(&self.config) {
                    self.record_activity(
                        ActivityLevel::Error,
                        "Config",
                        format!("Could not save the post-install route: {error}"),
                    );
                }
                self.restart_audio();
                self.rebuild_menu();
            }
            Err(error) => {
                if operation == DeviceOperation::Uninstall {
                    self.restart_audio();
                }
                self.audio_error = Some(error);
            }
        }
        self.refresh_ui();
    }

    fn rebuild_menu(&mut self) {
        match self.build_menu() {
            Ok((menu, ui)) => {
                if let Some(tray) = &self.tray {
                    tray.set_menu(Some(Box::new(menu)));
                }
                self.ui = Some(ui);
            }
            Err(error) => {
                self.audio_error = Some(format!("Could not rebuild menu: {error}"));
                self.record_activity(
                    ActivityLevel::Error,
                    "App",
                    format!("Menu rebuild failed: {error}"),
                );
            }
        }
    }

    fn show_window(&self) {
        if let Some(window) = &self.window {
            window.set_visible(true);
            window.set_focus();
        }
        self.refresh_ui();
    }

    fn hide_window(&self) {
        if let Some(window) = &self.window {
            window.set_visible(false);
        }
    }

    fn request_device_removal(&mut self, device_name: &str, action: &str) {
        self.record_activity(
            ActivityLevel::Info,
            "Installer",
            format!("Requested uninstall confirmation for {device_name}."),
        );
        self.show_window();
        let Some(webview) = &self.webview else {
            return;
        };
        let Ok(device_name) = serde_json::to_string(device_name) else {
            return;
        };
        let Ok(action) = serde_json::to_string(action) else {
            return;
        };
        let script = format!(
            "window.staticStream && \
             window.staticStream.confirmRemoval({action}, {device_name});"
        );
        if let Err(error) = webview.evaluate_script(&script) {
            tracing::warn!("could not show uninstall confirmation: {error}");
            self.record_activity(
                ActivityLevel::Error,
                "App",
                format!("Could not show uninstall confirmation: {error}"),
            );
            self.refresh_ui();
        }
    }

    fn is_running_from_applications() -> bool {
        std::env::current_exe().is_ok_and(|path| path.starts_with("/Applications/"))
    }

    fn camera_signing_ready() -> bool {
        let Some(executable) = std::env::current_exe().ok() else {
            return false;
        };
        let Ok(output) = Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(executable)
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| line.strip_prefix("TeamIdentifier="))
            .is_some_and(|team| !team.is_empty() && team != "not set")
    }

    fn refresh_ui(&self) {
        if let Some(ui) = &self.ui {
            ui.status.set_text(self.status_text());
            ui.check_updates
                .set_enabled(!self.update_checking && !self.update_installing);
            ui.camera.set_checked(self.state.camera_frozen);
            ui.camera.set_enabled(self.camera_available);
            ui.microphone.set_checked(self.state.microphone_muted);
            ui.microphone.set_enabled(self.audio.is_some());
            ui.replace_microphone
                .set_checked(self.config.replace_microphone_while_playing);
            ui.replace_microphone.set_enabled(self.audio.is_some());
            for (item, effect) in &ui.voice_effect_items {
                item.set_checked(self.config.voice_effect.effect == *effect);
                item.set_enabled(self.audio.is_some());
            }
            for (item, name) in &ui.input_items {
                item.set_checked(
                    self.selected_input_name()
                        .is_some_and(|selected| selected == name),
                );
            }
            for (item, name) in &ui.output_items {
                item.set_checked(
                    self.selected_output_name()
                        .is_some_and(|selected| selected == name),
                );
            }
        }

        if let Some(webview) = &self.webview {
            let state = self.gui_state();
            if let Ok(json) = serde_json::to_string(&state) {
                if let Ok(argument) = serde_json::to_string(&json) {
                    let script = format!(
                        "window.staticStream && window.staticStream.update(JSON.parse({argument}));"
                    );
                    if let Err(error) = webview.evaluate_script(&script) {
                        tracing::warn!("could not update control window: {error}");
                    }
                }
            }
        }
    }

    fn refresh_telemetry(&self) {
        let Some(webview) = &self.webview else {
            return;
        };
        let telemetry = GuiTelemetry {
            audio_levels: self.audio_levels.into(),
            clip_progress: self.clip_progress(),
        };
        if let Ok(json) = serde_json::to_string(&telemetry) {
            if let Ok(argument) = serde_json::to_string(&json) {
                let script = format!(
                    "window.staticStream && \
                     window.staticStream.updateTelemetry(JSON.parse({argument}));"
                );
                if let Err(error) = webview.evaluate_script(&script) {
                    tracing::warn!("could not update control telemetry: {error}");
                }
            }
        }
    }

    fn status_text(&self) -> String {
        if let Some(error) = &self.audio_error {
            return truncate_status(error);
        }
        if !self.camera_available {
            return "Setup required: virtual camera is unavailable".into();
        }
        match self.audio.as_ref().map(AudioEngine::ready_status) {
            Some(AudioEngineStatus::Ready { output, .. }) => {
                format!("{} | output: {output}", self.state.summary())
            }
            _ => self.state.summary().into(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn gui_state(&self) -> GuiState {
        let audio_ready = self.audio.is_some();
        let audio_driver_available = self.audio_driver_available();
        let audio_driver_installed = self.audio_driver_installed();
        let camera_message = if self.camera_available {
            if self.state.camera_frozen {
                "The last frame is being held for meeting apps.".into()
            } else {
                "The physical camera is passing through.".into()
            }
        } else {
            self.camera_status
                .clone()
                .unwrap_or_else(|| "Install the virtual camera before using Freeze.".into())
        };
        let audio_message = match self.audio.as_ref().map(AudioEngine::ready_status) {
            Some(AudioEngineStatus::Ready { input, output, .. }) => {
                format!("Routing {input} to {output}.")
            }
            _ => self
                .audio_error
                .clone()
                .unwrap_or_else(|| "Install the virtual microphone to start audio routing.".into()),
        };
        let camera_setup_detail = if self.camera_available {
            "Installed and available to meeting apps.".into()
        } else if self.camera_extension_registered {
            "Installed, but macOS is not currently publishing the camera.".into()
        } else if Self::camera_activation_helper().is_none() {
            "Unavailable when running the unbundled development binary.".into()
        } else if !self.camera_signing_ready {
            "Static Camera requires an Apple Development or Developer ID team signature. Rebuild with that identity to activate it.".into()
        } else if !Self::is_running_from_applications() {
            "Move Static Stream.app to /Applications before installation.".into()
        } else {
            self.camera_status
                .clone()
                .unwrap_or_else(|| "Not active. macOS approval is required once.".into())
        };
        let audio_setup_detail = if audio_driver_available {
            if audio_ready {
                "Installed and available to meeting apps.".into()
            } else {
                "Installed, but audio routing could not start.".into()
            }
        } else if self.legacy_audio_driver_available() {
            "An older Static Stream microphone is installed. Install/update to migrate it.".into()
        } else if audio_driver_installed {
            "Installed, but Core Audio is not currently publishing the microphone.".into()
        } else if Self::audio_installer().is_some() {
            "Not installed. Administrator approval is required once.".into()
        } else {
            "Unavailable when running the unbundled development binary.".into()
        };
        let speaker_monitor_name =
            self.audio
                .as_ref()
                .and_then(|audio| match audio.ready_status() {
                    AudioEngineStatus::Ready {
                        speaker_monitor: Some(name),
                        ..
                    } => Some(name.as_str()),
                    _ => None,
                });
        let speaker_monitor_message = if !self.config.play_clips_on_speakers {
            "Speaker playback is off.".into()
        } else if let Some(error) = &self.speaker_monitor_error {
            format!("Speaker playback unavailable: {error}")
        } else {
            speaker_monitor_name.map_or_else(
                || "Starting speaker playback...".into(),
                |name| format!("Clips also play through {name}."),
            )
        };

        GuiState {
            camera_available: self.camera_available,
            camera_frozen: self.state.camera_frozen,
            camera_extension_installed: self.camera_extension_registered,
            camera_installer_available: Self::camera_activation_helper().is_some()
                && self.camera_signing_ready,
            camera_signing_ready: self.camera_signing_ready,
            camera_message,
            camera_setup_detail,
            camera_test_available: Self::camera_development_test_helper().is_some(),
            camera_test_busy: self.camera_test_busy,
            camera_test_message: self.camera_test_status.clone().unwrap_or_else(|| {
                "The unsigned test exercises the same freeze selector used by Static Camera.".into()
            }),
            audio_driver_available,
            audio_driver_installed,
            audio_ready,
            audio_installer_available: Self::audio_installer().is_some(),
            audio_message,
            audio_setup_detail,
            microphone_muted: self.state.microphone_muted,
            replace_microphone: self.config.replace_microphone_while_playing,
            clip_gain: self.config.clip_gain,
            voice_effect: self.config.voice_effect.effect,
            voice_effect_intensity: self.config.voice_effect.intensity,
            voice_effect_mix: self.config.voice_effect.mix,
            play_clips_on_speakers: self.config.play_clips_on_speakers,
            speaker_gain: self.config.speaker_gain,
            speaker_monitor_ready: speaker_monitor_name.is_some(),
            speaker_monitor_message,
            audio_levels: self.audio_levels.into(),
            selected_input: self.selected_input_name().map(str::to_owned),
            selected_output: self.selected_output_name().map(str::to_owned),
            input_devices: self
                .devices
                .iter()
                .filter(|device| device.is_input && !device.is_probable_loopback)
                .map(|device| GuiNamedItem {
                    name: device.name.clone(),
                })
                .collect(),
            output_devices: self
                .devices
                .iter()
                .filter(|device| device.is_output && device.is_probable_loopback)
                .map(|device| GuiNamedItem {
                    name: device.name.clone(),
                })
                .collect(),
            clips: self
                .clips
                .iter()
                .map(|path| GuiNamedItem {
                    name: path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                })
                .collect(),
            clip_phase: self.clip_phase,
            clip_name: self.clip_name.clone(),
            clip_message: if !audio_ready && self.clip_phase == ClipPhase::Idle {
                "Install Static Microphone before playing sound clips.".into()
            } else {
                self.clip_message.clone()
            },
            clip_progress: self.clip_progress(),
            current_version: env!("CARGO_PKG_VERSION"),
            auto_check_updates: self.config.auto_check_updates,
            update_checking: self.update_checking,
            update_installing: self.update_installing,
            update_install_supported: self.update_install_unavailable_reason.is_none(),
            update_available_version: self
                .update_available
                .as_ref()
                .map(|manifest| manifest.version.clone()),
            update_notes: self
                .update_available
                .as_ref()
                .map_or_else(String::new, |manifest| manifest.notes.clone()),
            update_status: self.update_status.clone(),
            activity_events: self.activity.snapshot(),
            busy: self.installer_busy,
        }
    }

    fn clip_progress(&self) -> f32 {
        normalized_clip_progress(
            self.clip_phase,
            self.clip_started_at.map(|started_at| started_at.elapsed()),
            self.clip_duration,
        )
    }

    fn audio_driver_available(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.name == STATIC_MICROPHONE)
    }

    fn legacy_audio_driver_available(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.name == LEGACY_STATIC_MICROPHONE)
    }

    fn audio_driver_installed(&self) -> bool {
        AUDIO_DRIVER_PATHS
            .iter()
            .any(|path| Path::new(path).exists())
            || self
                .devices
                .iter()
                .any(|device| is_static_audio_device_name(&device.name))
    }

    fn selected_input_name(&self) -> Option<&str> {
        self.config
            .input_device
            .as_deref()
            .filter(|selected| {
                self.devices.iter().any(|device| {
                    device.name == *selected && device.is_input && !device.is_probable_loopback
                })
            })
            .or_else(|| {
                self.audio
                    .as_ref()
                    .and_then(|audio| match audio.ready_status() {
                        AudioEngineStatus::Ready { input, .. } => Some(input.as_str()),
                        _ => None,
                    })
            })
    }

    fn selected_output_name(&self) -> Option<&str> {
        self.config
            .output_device
            .as_deref()
            .filter(|selected| {
                self.devices.iter().any(|device| {
                    device.name == *selected && device.is_output && device.is_probable_loopback
                })
            })
            .or_else(|| {
                self.audio
                    .as_ref()
                    .and_then(|audio| match audio.ready_status() {
                        AudioEngineStatus::Ready { output, .. } => Some(output.as_str()),
                        _ => None,
                    })
            })
    }
}

fn shortcut_label(action: ShortcutAction) -> String {
    match action {
        ShortcutAction::ToggleCamera => "toggle camera freeze".into(),
        ShortcutAction::ToggleMicrophone => "toggle microphone mute".into(),
        ShortcutAction::CycleVoiceEffect => "select the next voice effect".into(),
        ShortcutAction::StopClips => "stop sound clip".into(),
        ShortcutAction::PlayClip(index) => format!("play sound clip {}", index + 1),
    }
}

fn gui_command_message(command: &GuiCommand) -> String {
    match command {
        GuiCommand::Ready => "Control window connected.".into(),
        GuiCommand::SetCamera { frozen } => format!(
            "Window requested camera mode: {}.",
            if *frozen { "frozen" } else { "live" }
        ),
        GuiCommand::SetMuted { muted } => format!(
            "Window requested microphone mode: {}.",
            if *muted { "muted" } else { "live" }
        ),
        GuiCommand::SetReplace { enabled } => format!(
            "Window requested clip mode: {}.",
            if *enabled { "replace" } else { "mix" }
        ),
        GuiCommand::SetClipGain { gain } => {
            format!("Window requested virtual clip volume {:.0}%.", gain * 100.0)
        }
        GuiCommand::SetVoiceEffect { effect } => {
            format!("Window requested the {} voice effect.", effect.label())
        }
        GuiCommand::SetVoiceIntensity { intensity } => {
            format!(
                "Window requested voice intensity {:.0}%.",
                intensity * 100.0
            )
        }
        GuiCommand::SetVoiceMix { mix } => {
            format!("Window requested voice effect mix {:.0}%.", mix * 100.0)
        }
        GuiCommand::SetSpeakerMonitor { enabled } => format!(
            "Window requested speaker playback {}.",
            if *enabled { "enabled" } else { "disabled" }
        ),
        GuiCommand::SetSpeakerGain { gain } => {
            format!("Window requested speaker clip volume {:.0}%.", gain * 100.0)
        }
        GuiCommand::SelectInput { name } => {
            format!("Window selected physical microphone: {name}.")
        }
        GuiCommand::SelectOutput { name } => {
            format!("Window selected virtual microphone output: {name}.")
        }
        GuiCommand::PlayClip { index } => {
            format!("Window requested sound clip {}.", index + 1)
        }
        GuiCommand::StopClip => "Window requested sound clip stop.".into(),
        GuiCommand::OpenClips => "Window requested the Clips folder.".into(),
        GuiCommand::RefreshClips => "Window requested a sound clip refresh.".into(),
        GuiCommand::Refresh => "Window requested a device and clip refresh.".into(),
        GuiCommand::TestCamera => "Window requested the unsigned camera freeze test.".into(),
        GuiCommand::InstallCamera => "Window requested Static Camera install/update.".into(),
        GuiCommand::InstallAudio => "Window requested Static Microphone install/update.".into(),
        GuiCommand::UninstallCamera => "Window confirmed Static Camera uninstall.".into(),
        GuiCommand::UninstallAudio => "Window confirmed Static Microphone uninstall.".into(),
        GuiCommand::SetAutoCheckUpdates { enabled } => format!(
            "Window requested automatic update checks {}.",
            if *enabled { "enabled" } else { "disabled" }
        ),
        GuiCommand::CheckForUpdates => "Window requested an update check.".into(),
        GuiCommand::InstallUpdate => "Window requested update installation.".into(),
        GuiCommand::ClearActivity => "Window requested activity log clear.".into(),
    }
}

fn audio_ready_message(status: &AudioEngineStatus) -> String {
    match status {
        AudioEngineStatus::Ready {
            input,
            output,
            sample_rate,
            channels,
            speaker_monitor,
        } => {
            let monitor = speaker_monitor
                .as_ref()
                .map_or_else(String::new, |name| format!("; clips -> {name}"));
            format!(
                "Audio routing started: {input} -> {output}, {sample_rate} Hz, {channels} channel(s){monitor}."
            )
        }
        AudioEngineStatus::StreamError(error) => format!("Audio routing failed: {error}"),
        AudioEngineStatus::SpeakerMonitorError(error) => {
            format!("Speaker monitoring failed: {error}")
        }
        AudioEngineStatus::ClipStarted { .. }
        | AudioEngineStatus::ClipFinished { .. }
        | AudioEngineStatus::ClipStopped { .. }
        | AudioEngineStatus::VoiceEffectChanged(_) => "Audio processing state changed.".into(),
    }
}

fn audio_command_name(command: &AudioCommand) -> String {
    match command {
        AudioCommand::SetMuted(muted) => {
            format!(
                "Microphone {} command",
                if *muted { "mute" } else { "unmute" }
            )
        }
        AudioCommand::SetClipGain(gain) => {
            format!("Set virtual clip volume to {:.0}% command", gain * 100.0)
        }
        AudioCommand::SetSpeakerGain(gain) => {
            format!("Set speaker clip volume to {:.0}% command", gain * 100.0)
        }
        AudioCommand::SetVoiceEffect(settings) => {
            format!("Set {} voice effect command", settings.effect.label())
        }
        AudioCommand::Play { clip, .. } => format!("Play {} command", clip.name),
        AudioCommand::Stop => "Stop clip command".into(),
    }
}

fn clip_display_name(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn normalized_clip_progress(
    phase: ClipPhase,
    elapsed: Option<Duration>,
    duration: Option<Duration>,
) -> f32 {
    if phase == ClipPhase::Finished {
        return 1.0;
    }
    if phase != ClipPhase::Playing {
        return 0.0;
    }
    let (Some(elapsed), Some(duration)) = (elapsed, duration) else {
        return 0.0;
    };
    if duration.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

const fn telemetry_interval(window_visible: bool, audio_ready: bool) -> Duration {
    if window_visible && audio_ready {
        VISIBLE_TELEMETRY_INTERVAL
    } else {
        HIDDEN_TELEMETRY_INTERVAL
    }
}

fn menu_accelerator(code: MenuCode) -> Accelerator {
    Accelerator::new(Some(MenuModifiers::SUPER | MenuModifiers::ALT), code)
}

fn digit_code(index: usize) -> Option<MenuCode> {
    [
        MenuCode::Digit1,
        MenuCode::Digit2,
        MenuCode::Digit3,
        MenuCode::Digit4,
        MenuCode::Digit5,
        MenuCode::Digit6,
        MenuCode::Digit7,
        MenuCode::Digit8,
        MenuCode::Digit9,
    ]
    .get(index)
    .copied()
}

fn is_static_audio_device_name(name: &str) -> bool {
    matches!(name, STATIC_MICROPHONE | LEGACY_STATIC_MICROPHONE)
}

fn camera_extension_registered() -> bool {
    Command::new("/usr/bin/systemextensionsctl")
        .arg("list")
        .output()
        .is_ok_and(|output| {
            system_extension_listing_contains(&output.stdout, CAMERA_EXTENSION_ID)
                || system_extension_listing_contains(&output.stderr, CAMERA_EXTENSION_ID)
        })
}

fn system_extension_listing_contains(output: &[u8], identifier: &str) -> bool {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .any(|field| field == identifier)
}

fn truncate_status(status: &str) -> String {
    let mut characters = status.chars();
    let short: String = characters.by_ref().take(96).collect();
    if characters.next().is_some() {
        format!("{short}...")
    } else {
        short
    }
}

fn status_icon() -> anyhow::Result<Icon> {
    const SIZE: usize = 18;
    let mut rgba = vec![0_u8; SIZE * SIZE * 4];
    let mut pixel = |x: usize, y: usize| {
        let offset = (y * SIZE + x) * 4;
        rgba[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]);
    };

    for x in 2..13 {
        pixel(x, 4);
        pixel(x, 13);
    }
    for y in 4..14 {
        pixel(2, y);
        pixel(12, y);
    }
    for y in 7..11 {
        pixel(15, y);
    }
    pixel(13, 7);
    pixel(14, 6);
    pixel(13, 10);
    pixel(14, 11);
    for y in 7..11 {
        pixel(6, y);
        pixel(9, y);
    }

    Icon::from_rgba(rgba, SIZE as u32, SIZE as u32).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_truncation_is_unicode_boundary_safe() {
        let long = "camera".repeat(30);
        let status = truncate_status(&long);
        assert!(status.ends_with("..."));
        assert_eq!(status.chars().count(), 99);
    }

    #[test]
    fn first_nine_clips_have_shortcuts() {
        assert_eq!(digit_code(0), Some(MenuCode::Digit1));
        assert_eq!(digit_code(8), Some(MenuCode::Digit9));
        assert_eq!(digit_code(9), None);
    }

    #[test]
    fn only_owned_audio_device_names_are_recognized() {
        assert!(is_static_audio_device_name("Static Microphone"));
        assert!(is_static_audio_device_name("Static Stream Microphone"));
        assert!(!is_static_audio_device_name("BlackHole 2ch"));
        assert!(!is_static_audio_device_name("Static Microphone Copy"));
    }

    #[test]
    fn camera_registration_matches_the_exact_bundle_identifier() {
        let listing = b"* * com.madpin.staticstream.camera (enabled active)";
        assert!(system_extension_listing_contains(
            listing,
            CAMERA_EXTENSION_ID
        ));
        assert!(!system_extension_listing_contains(
            b"* * com.madpin.staticstream.camera.old",
            CAMERA_EXTENSION_ID
        ));
    }

    #[test]
    fn activity_log_is_bounded_and_newest_first() {
        let mut log = ActivityLog::new();
        for index in 0..=MAX_ACTIVITY_EVENTS {
            log.push(ActivityLevel::Info, "Test", format!("event {index}"));
        }

        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), MAX_ACTIVITY_EVENTS);
        assert_eq!(snapshot.first().map(|event| event.id), Some(251));
        assert_eq!(snapshot.last().map(|event| event.id), Some(2));
    }

    #[test]
    fn clearing_activity_removes_all_entries() {
        let mut log = ActivityLog::new();
        log.push(ActivityLevel::Warning, "Test", "warning");
        log.clear();
        assert!(log.snapshot().is_empty());
    }

    #[test]
    fn clip_progress_tracks_playback_and_terminal_states() {
        assert_eq!(
            normalized_clip_progress(
                ClipPhase::Playing,
                Some(Duration::from_millis(250)),
                Some(Duration::from_secs(1)),
            ),
            0.25
        );
        assert_eq!(
            normalized_clip_progress(
                ClipPhase::Playing,
                Some(Duration::from_secs(2)),
                Some(Duration::from_secs(1)),
            ),
            1.0
        );
        assert_eq!(
            normalized_clip_progress(ClipPhase::Finished, None, None),
            1.0
        );
        assert_eq!(
            normalized_clip_progress(
                ClipPhase::Stopped,
                Some(Duration::from_millis(250)),
                Some(Duration::from_secs(1)),
            ),
            0.0
        );
    }

    #[test]
    fn telemetry_is_fast_only_for_a_visible_ready_window() {
        assert_eq!(telemetry_interval(true, true), Duration::from_millis(100));
        assert_eq!(telemetry_interval(false, true), Duration::from_millis(500));
        assert_eq!(telemetry_interval(true, false), Duration::from_millis(500));
    }
}
