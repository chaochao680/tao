use openharmony_ability::xcomponent::KeyCode as Keycode;

use crate::keyboard::{Key, KeyLocation, NativeKeyCode};

pub fn to_logical(keycode: Keycode) -> Key<'static> {
  use openharmony_ability::xcomponent::KeyCode::*;

  let native = NativeKeyCode::Ohos(i32::from(keycode));

  match keycode {
    // Using `BrowserHome` instead of `GoHome` according to
    // https://developer.mozilla.org/en-US/docs/Web/API/KeyboardEvent/key/Key_Values
    Home => Key::BrowserHome,
    Back => Key::BrowserBack,

    //-------------------------------------------------------------------------------
    // These should be redundant because they should have already been matched
    // as `KeyMapChar::Unicode`, but also matched here as a fallback
    Key0 => Key::Unidentified(native),
    Key1 => Key::Unidentified(native),
    Key2 => Key::Unidentified(native),
    Key3 => Key::Unidentified(native),
    Key4 => Key::Unidentified(native),
    Key5 => Key::Unidentified(native),
    Key6 => Key::Unidentified(native),
    Key7 => Key::Unidentified(native),
    Key8 => Key::Unidentified(native),
    Key9 => Key::Unidentified(native),
    Star => Key::Unidentified(native),
    Pound => Key::Unidentified(native),
    A => Key::Unidentified(native),
    B => Key::Unidentified(native),
    C => Key::Unidentified(native),
    D => Key::Unidentified(native),
    E => Key::Unidentified(native),
    F => Key::Unidentified(native),
    G => Key::Unidentified(native),
    H => Key::Unidentified(native),
    I => Key::Unidentified(native),
    J => Key::Unidentified(native),
    K => Key::Unidentified(native),
    L => Key::Unidentified(native),
    M => Key::Unidentified(native),
    N => Key::Unidentified(native),
    O => Key::Unidentified(native),
    P => Key::Unidentified(native),
    Q => Key::Unidentified(native),
    R => Key::Unidentified(native),
    S => Key::Unidentified(native),
    T => Key::Unidentified(native),
    U => Key::Unidentified(native),
    V => Key::Unidentified(native),
    W => Key::Unidentified(native),
    X => Key::Unidentified(native),
    Y => Key::Unidentified(native),
    Z => Key::Unidentified(native),
    Comma => Key::Unidentified(native),
    Period => Key::Unidentified(native),
    Grave => Key::Unidentified(native),
    Minus => Key::Unidentified(native),
    Equals => Key::Unidentified(native),
    LeftBracket => Key::Unidentified(native),
    RightBracket => Key::Unidentified(native),
    Backslash => Key::Unidentified(native),
    Semicolon => Key::Unidentified(native),
    Apostrophe => Key::Unidentified(native),
    Slash => Key::Unidentified(native),
    At => Key::Unidentified(native),
    Plus => Key::Unidentified(native),
    //-------------------------------------------------------------------------------
    DpadUp => Key::ArrowUp,
    DpadDown => Key::ArrowDown,
    DpadLeft => Key::ArrowLeft,
    DpadRight => Key::ArrowRight,
    DpadCenter => Key::Enter,

    VolumeUp => Key::AudioVolumeUp,
    VolumeDown => Key::AudioVolumeDown,
    Power => Key::Power,
    Camera => Key::Camera,
    // Clear => Key::Named(NamedKey::Clear),
    AltLeft => Key::Alt,
    AltRight => Key::Alt,
    ShiftLeft => Key::Shift,
    ShiftRight => Key::Shift,
    Tab => Key::Tab,
    Space => Key::Space,
    Sym => Key::Symbol,
    Explorer => Key::LaunchWebBrowser,
    Envelope => Key::LaunchMail,
    Enter => Key::Enter,
    Del => Key::Backspace,

    // According to https://developer.android.com/reference/android/view/KeyEvent#KEYCODE_NUM
    // Num => Key::Named(NamedKey::Alt),

    // Headsethook => Key::Named(NamedKey::HeadsetHook),
    // Focus => Key::Named(NamedKey::CameraFocus),

    // Notification => Key::Named(NamedKey::Notification),
    // Search => Key::Named(NamedKey::BrowserSearch),
    MediaPlayPause => Key::MediaPlayPause,
    MediaStop => Key::MediaStop,
    MediaNext => Key::MediaTrackNext,
    MediaPrevious => Key::MediaTrackPrevious,
    MediaRewind => Key::MediaRewind,
    MediaFastForward => Key::MediaFastForward,
    Mute => Key::MicrophoneVolumeMute,
    PageUp => Key::PageUp,
    PageDown => Key::PageDown,

    Escape => Key::Escape,
    ForwardDel => Key::Delete,
    CtrlLeft => Key::Control,
    CtrlRight => Key::Control,
    CapsLock => Key::CapsLock,
    ScrollLock => Key::ScrollLock,
    MetaLeft => Key::Super,
    MetaRight => Key::Super,
    Function => Key::Fn,
    SysRq => Key::PrintScreen,
    Break => Key::Pause,
    MoveHome => Key::Home,
    MoveEnd => Key::End,
    Insert => Key::Insert,
    Forward => Key::BrowserForward,
    MediaPlay => Key::MediaPlay,
    MediaPause => Key::MediaPause,
    MediaClose => Key::MediaClose,
    MediaEject => Key::Eject,
    MediaRecord => Key::MediaRecord,
    F1 => Key::F1,
    F2 => Key::F2,
    F3 => Key::F3,
    F4 => Key::F4,
    F5 => Key::F5,
    F6 => Key::F6,
    F7 => Key::F7,
    F8 => Key::F8,
    F9 => Key::F9,
    F10 => Key::F10,
    F11 => Key::F11,
    F12 => Key::F12,
    NumLock => Key::NumLock,
    Numpad0 => Key::Unidentified(native),
    Numpad1 => Key::Unidentified(native),
    Numpad2 => Key::Unidentified(native),
    Numpad3 => Key::Unidentified(native),
    Numpad4 => Key::Unidentified(native),
    Numpad5 => Key::Unidentified(native),
    Numpad6 => Key::Unidentified(native),
    Numpad7 => Key::Unidentified(native),
    Numpad8 => Key::Unidentified(native),
    Numpad9 => Key::Unidentified(native),
    NumpadDivide => Key::Unidentified(native),
    NumpadMultiply => Key::Unidentified(native),
    NumpadSubtract => Key::Unidentified(native),
    NumpadAdd => Key::Unidentified(native),
    NumpadDot => Key::Unidentified(native),
    NumpadComma => Key::Unidentified(native),
    NumpadEnter => Key::Unidentified(native),
    NumpadEquals => Key::Unidentified(native),
    NumpadLeftParen => Key::Unidentified(native),
    NumpadRightParen => Key::Unidentified(native),

    VolumeMute => Key::AudioVolumeMute,
    Info => Key::Info,
    ChannelUp => Key::ChannelUp,
    ChannelDown => Key::ChannelDown,
    ZoomIn => Key::ZoomIn,
    ZoomOut => Key::ZoomOut,
    TV => Key::TV,
    // Guide => Key::Named(NamedKey::Guide),
    // Dvr => Key::Named(NamedKey::DVR),
    // Bookmark => Key::Named(NamedKey::BrowserFavorites),
    // Captions => Key::Named(NamedKey::ClosedCaptionToggle),
    // Settings => Key::Named(NamedKey::Settings),
    // TvPower => Key::Named(NamedKey::TVPower),
    // TvInput => Key::Named(NamedKey::TVInput),
    // StbPower => Key::Named(NamedKey::STBPower),
    // StbInput => Key::Named(NamedKey::STBInput),
    // AvrPower => Key::Named(NamedKey::AVRPower),
    // AvrInput => Key::Named(NamedKey::AVRInput),
    // ProgRed => Key::Named(NamedKey::ColorF0Red),
    // ProgGreen => Key::Named(NamedKey::ColorF1Green),
    // ProgYellow => Key::Named(NamedKey::ColorF2Yellow),
    // ProgBlue => Key::Named(NamedKey::ColorF3Blue),
    // AppSwitch => Key::Named(NamedKey::AppSwitch),
    // LanguageSwitch => Key::Named(NamedKey::GroupNext),
    // MannerMode => Key::Named(NamedKey::MannerMode),
    // Keycode3dMode => Key::Named(NamedKey::TV3DMode),
    // Contacts => Key::Named(NamedKey::LaunchContacts),
    Calendar => Key::LaunchCalendar,
    // Music => Key::Named(NamedKey::LaunchMusicPlayer),
    // Calculator => Key::Named(NamedKey::LaunchApplication2),
    ZenkakuHankaku => Key::ZenkakuHankaku,
    // Eisu => Key::Named(NamedKey::Eisu),
    Muhenkan => Key::NonConvert,
    Henkan => Key::Convert,
    KatakanaHiragana => Key::HiraganaKatakana,
    // Kana => Key::Named(NamedKey::KanjiMode),
    BrightnessDown => Key::BrightnessDown,
    BrightnessUp => Key::BrightnessUp,
    // MediaAudioTrack => Key::Named(NamedKey::MediaAudioTrack),
    Sleep => Key::Standby,
    Wakeup => Key::WakeUp,
    // Pairing => Key::Named(NamedKey::Pairing),
    // MediaTopMenu => Key::Named(NamedKey::MediaTopMenu),
    // LastChannel => Key::Named(NamedKey::MediaLast),
    // TvDataService => Key::Named(NamedKey::TVDataService),
    // VoiceAssist => Key::Named(NamedKey::VoiceDial),
    // TvRadioService => Key::Named(NamedKey::TVRadioService),
    // TvTeletext => Key::Named(NamedKey::Teletext),
    // TvNumberEntry => Key::Named(NamedKey::TVNumberEntry),
    // TvTerrestrialAnalog => Key::Named(NamedKey::TVTerrestrialAnalog),
    // TvTerrestrialDigital => Key::Named(NamedKey::TVTerrestrialDigital),
    // TvSatellite => Key::Named(NamedKey::TVSatellite),
    // TvSatelliteBs => Key::Named(NamedKey::TVSatelliteBS),
    // TvSatelliteCs => Key::Named(NamedKey::TVSatelliteCS),
    // TvSatelliteService => Key::Named(NamedKey::TVSatelliteToggle),
    // TvNetwork => Key::Named(NamedKey::TVNetwork),
    // TvAntennaCable => Key::Named(NamedKey::TVAntennaCable),
    // TvInputHdmi1 => Key::Named(NamedKey::TVInputHDMI1),
    // TvInputHdmi2 => Key::Named(NamedKey::TVInputHDMI2),
    // TvInputHdmi3 => Key::Named(NamedKey::TVInputHDMI3),
    // TvInputHdmi4 => Key::Named(NamedKey::TVInputHDMI4),
    // TvInputComposite1 => Key::Named(NamedKey::TVInputComposite1),
    // TvInputComposite2 => Key::Named(NamedKey::TVInputComposite2),
    // TvInputComponent1 => Key::Named(NamedKey::TVInputComponent1),
    // TvInputComponent2 => Key::Named(NamedKey::TVInputComponent2),
    // TvInputVga1 => Key::Named(NamedKey::TVInputVGA1),
    // TvAudioDescription => Key::Named(NamedKey::TVAudioDescription),
    // TvAudioDescriptionMixUp => Key::Named(NamedKey::TVAudioDescriptionMixUp),
    // TvAudioDescriptionMixDown => Key::Named(NamedKey::TVAudioDescriptionMixDown),
    // TvZoomMode => Key::Named(NamedKey::ZoomToggle),
    // TvContentsMenu => Key::Named(NamedKey::TVContentsMenu),
    // TvMediaContextMenu => Key::Named(NamedKey::TVMediaContext),
    // TvTimerProgramming => Key::Named(NamedKey::TVTimer),
    Help => Key::Help,
    // NavigatePrevious => Key::Named(NamedKey::NavigatePrevious),
    // NavigateNext => Key::Named(NamedKey::NavigateNext),
    // NavigateIn => Key::Named(NamedKey::NavigateIn),
    // NavigateOut => Key::Named(NamedKey::NavigateOut),
    // MediaSkipForward => Key::Named(NamedKey::MediaSkipForward),
    // MediaSkipBackward => Key::Named(NamedKey::MediaSkipBackward),
    // MediaStepForward => Key::Named(NamedKey::MediaStepForward),
    // MediaStepBackward => Key::Named(NamedKey::MediaStepBackward),
    Cut => Key::Cut,
    Copy => Key::Copy,
    Paste => Key::Paste,
    Refresh => Key::BrowserRefresh,

    // -----------------------------------------------------------------
    // Keycodes that don't have a logical Key mapping
    // -----------------------------------------------------------------
    Unknown => Key::Unidentified(native),

    // Can be added on demand
    // SoftLeft => Key::Unidentified(native),
    // SoftRight => Key::Unidentified(native),
    Menu => Key::Unidentified(native),

    // Pictsymbols => Key::Unidentified(native),
    // SwitchCharset => Key::Unidentified(native),

    // -----------------------------------------------------------------
    // Gamepad events should be exposed through a separate API, not
    // keyboard events
    // ButtonA => Key::Unidentified(native),
    // ButtonB => Key::Unidentified(native),
    // ButtonC => Key::Unidentified(native),
    // ButtonX => Key::Unidentified(native),
    // ButtonY => Key::Unidentified(native),
    // ButtonZ => Key::Unidentified(native),
    // ButtonL1 => Key::Unidentified(native),
    // ButtonR1 => Key::Unidentified(native),
    // ButtonL2 => Key::Unidentified(native),
    // ButtonR2 => Key::Unidentified(native),
    // ButtonThumbl => Key::Unidentified(native),
    // ButtonThumbr => Key::Unidentified(native),
    // ButtonStart => Key::Unidentified(native),
    // ButtonSelect => Key::Unidentified(native),
    // ButtonMode => Key::Unidentified(native),
    // // -----------------------------------------------------------------
    // Window => Key::Unidentified(native),

    // Button1 => Key::Unidentified(native),
    // Button2 => Key::Unidentified(native),
    // Button3 => Key::Unidentified(native),
    // Button4 => Key::Unidentified(native),
    // Button5 => Key::Unidentified(native),
    // Button6 => Key::Unidentified(native),
    // Button7 => Key::Unidentified(native),
    // Button8 => Key::Unidentified(native),
    // Button9 => Key::Unidentified(native),
    // Button10 => Key::Unidentified(native),
    // Button11 => Key::Unidentified(native),
    // Button12 => Key::Unidentified(native),
    // Button13 => Key::Unidentified(native),
    // Button14 => Key::Unidentified(native),
    // Button15 => Key::Unidentified(native),
    // Button16 => Key::Unidentified(native),
    Yen => Key::Unidentified(native),
    Ro => Key::Unidentified(native),

    // Assist => Key::Unidentified(native),

    // Keycode11 => Key::Unidentified(native),
    // Keycode12 => Key::Unidentified(native),

    // StemPrimary => Key::Unidentified(native),
    // Stem1 => Key::Unidentified(native),
    // Stem2 => Key::Unidentified(native),
    // Stem3 => Key::Unidentified(native),

    // DpadUpLeft => Key::Unidentified(native),
    // DpadDownLeft => Key::Unidentified(native),
    // DpadUpRight => Key::Unidentified(native),
    // DpadDownRight => Key::Unidentified(native),

    // SoftSleep => Key::Unidentified(native),

    // SystemNavigationUp => Key::Unidentified(native),
    // SystemNavigationDown => Key::Unidentified(native),
    // SystemNavigationLeft => Key::Unidentified(native),
    // SystemNavigationRight => Key::Unidentified(native),

    // AllApps => Key::Unidentified(native),
    // ThumbsUp => Key::Unidentified(native),
    // ThumbsDown => Key::Unidentified(native),
    // ProfileSwitch => Key::Unidentified(native),

    // It's always possible that new versions of Android could introduce
    // key codes we can't know about at compile time.
    _ => Key::Unidentified(native),
  }
}

pub fn to_location(keycode: Keycode) -> KeyLocation {
  use openharmony_ability::xcomponent::KeyCode::*;

  match keycode {
    AltLeft => KeyLocation::Left,
    AltRight => KeyLocation::Right,
    ShiftLeft => KeyLocation::Left,
    ShiftRight => KeyLocation::Right,

    CtrlLeft => KeyLocation::Left,
    CtrlRight => KeyLocation::Right,
    MetaLeft => KeyLocation::Left,
    MetaRight => KeyLocation::Right,

    NumLock => KeyLocation::Numpad,
    Numpad0 => KeyLocation::Numpad,
    Numpad1 => KeyLocation::Numpad,
    Numpad2 => KeyLocation::Numpad,
    Numpad3 => KeyLocation::Numpad,
    Numpad4 => KeyLocation::Numpad,
    Numpad5 => KeyLocation::Numpad,
    Numpad6 => KeyLocation::Numpad,
    Numpad7 => KeyLocation::Numpad,
    Numpad8 => KeyLocation::Numpad,
    Numpad9 => KeyLocation::Numpad,
    NumpadDivide => KeyLocation::Numpad,
    NumpadMultiply => KeyLocation::Numpad,
    NumpadSubtract => KeyLocation::Numpad,
    NumpadAdd => KeyLocation::Numpad,
    NumpadDot => KeyLocation::Numpad,
    NumpadComma => KeyLocation::Numpad,
    NumpadEnter => KeyLocation::Numpad,
    NumpadEquals => KeyLocation::Numpad,
    NumpadLeftParen => KeyLocation::Numpad,
    NumpadRightParen => KeyLocation::Numpad,

    _ => KeyLocation::Standard,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use openharmony_ability::xcomponent::KeyCode::*;

  // ─── to_logical: navigation & media ────────────────────────────────────

  #[test]
  fn to_logical_home_maps_to_browser_home() {
    assert!(matches!(to_logical(Home), Key::BrowserHome));
  }

  #[test]
  fn to_logical_back_maps_to_browser_back() {
    assert!(matches!(to_logical(Back), Key::BrowserBack));
  }

  #[test]
  fn to_logical_dpad_navigation() {
    assert!(matches!(to_logical(DpadUp), Key::ArrowUp));
    assert!(matches!(to_logical(DpadDown), Key::ArrowDown));
    assert!(matches!(to_logical(DpadLeft), Key::ArrowLeft));
    assert!(matches!(to_logical(DpadRight), Key::ArrowRight));
    assert!(matches!(to_logical(DpadCenter), Key::Enter));
  }

  #[test]
  fn to_logical_volume_keys() {
    assert!(matches!(to_logical(VolumeUp), Key::AudioVolumeUp));
    assert!(matches!(to_logical(VolumeDown), Key::AudioVolumeDown));
    assert!(matches!(to_logical(VolumeMute), Key::AudioVolumeMute));
  }

  #[test]
  fn to_logical_media_keys() {
    assert!(matches!(to_logical(MediaPlayPause), Key::MediaPlayPause));
    assert!(matches!(to_logical(MediaStop), Key::MediaStop));
    assert!(matches!(to_logical(MediaNext), Key::MediaTrackNext));
    assert!(matches!(to_logical(MediaPrevious), Key::MediaTrackPrevious));
    assert!(matches!(to_logical(MediaRewind), Key::MediaRewind));
    assert!(matches!(to_logical(MediaFastForward), Key::MediaFastForward));
    assert!(matches!(to_logical(MediaPlay), Key::MediaPlay));
    assert!(matches!(to_logical(MediaPause), Key::MediaPause));
    assert!(matches!(to_logical(MediaClose), Key::MediaClose));
    assert!(matches!(to_logical(MediaEject), Key::Eject));
    assert!(matches!(to_logical(MediaRecord), Key::MediaRecord));
  }

  #[test]
  fn to_logical_mute_maps_to_microphone_volume_mute() {
    assert!(matches!(to_logical(Mute), Key::MicrophoneVolumeMute));
  }

  // ─── to_logical: modifier keys ────────────────────────────────────────

  #[test]
  fn to_logical_alt_keys() {
    assert!(matches!(to_logical(AltLeft), Key::Alt));
    assert!(matches!(to_logical(AltRight), Key::Alt));
  }

  #[test]
  fn to_logical_shift_keys() {
    assert!(matches!(to_logical(ShiftLeft), Key::Shift));
    assert!(matches!(to_logical(ShiftRight), Key::Shift));
  }

  #[test]
  fn to_logical_ctrl_keys() {
    assert!(matches!(to_logical(CtrlLeft), Key::Control));
    assert!(matches!(to_logical(CtrlRight), Key::Control));
  }

  #[test]
  fn to_logical_meta_keys() {
    assert!(matches!(to_logical(MetaLeft), Key::Super));
    assert!(matches!(to_logical(MetaRight), Key::Super));
  }

  #[test]
  fn to_logical_caps_scroll_num_lock() {
    assert!(matches!(to_logical(CapsLock), Key::CapsLock));
    assert!(matches!(to_logical(ScrollLock), Key::ScrollLock));
    assert!(matches!(to_logical(NumLock), Key::NumLock));
  }

  // ─── to_logical: common keys ──────────────────────────────────────────

  #[test]
  fn to_logical_tab_space_enter() {
    assert!(matches!(to_logical(Tab), Key::Tab));
    assert!(matches!(to_logical(Space), Key::Space));
    assert!(matches!(to_logical(Enter), Key::Enter));
  }

  #[test]
  fn to_logical_del_maps_to_backspace() {
    assert!(matches!(to_logical(Del), Key::Backspace));
  }

  #[test]
  fn to_logical_forward_del_maps_to_delete() {
    assert!(matches!(to_logical(ForwardDel), Key::Delete));
  }

  #[test]
  fn to_logical_escape() {
    assert!(matches!(to_logical(Escape), Key::Escape));
  }

  #[test]
  fn to_logical_function_key() {
    assert!(matches!(to_logical(Function), Key::Fn));
  }

  // ─── to_logical: F-keys ───────────────────────────────────────────────

  #[test]
  fn to_logical_f1_through_f12() {
    assert!(matches!(to_logical(F1), Key::F1));
    assert!(matches!(to_logical(F2), Key::F2));
    assert!(matches!(to_logical(F3), Key::F3));
    assert!(matches!(to_logical(F4), Key::F4));
    assert!(matches!(to_logical(F5), Key::F5));
    assert!(matches!(to_logical(F6), Key::F6));
    assert!(matches!(to_logical(F7), Key::F7));
    assert!(matches!(to_logical(F8), Key::F8));
    assert!(matches!(to_logical(F9), Key::F9));
    assert!(matches!(to_logical(F10), Key::F10));
    assert!(matches!(to_logical(F11), Key::F11));
    assert!(matches!(to_logical(F12), Key::F12));
  }

  // ─── to_logical: page nav & editing ───────────────────────────────────

  #[test]
  fn to_logical_page_up_down() {
    assert!(matches!(to_logical(PageUp), Key::PageUp));
    assert!(matches!(to_logical(PageDown), Key::PageDown));
  }

  #[test]
  fn to_logical_move_home_end_insert() {
    assert!(matches!(to_logical(MoveHome), Key::Home));
    assert!(matches!(to_logical(MoveEnd), Key::End));
    assert!(matches!(to_logical(Insert), Key::Insert));
  }

  #[test]
  fn to_logical_forward_maps_to_browser_forward() {
    assert!(matches!(to_logical(Forward), Key::BrowserForward));
  }

  #[test]
  fn to_logical_sysrq_break() {
    assert!(matches!(to_logical(SysRq), Key::PrintScreen));
    assert!(matches!(to_logical(Break), Key::Pause));
  }

  // ─── to_logical: special buttons ──────────────────────────────────────

  #[test]
  fn to_logical_power_camera() {
    assert!(matches!(to_logical(Power), Key::Power));
    assert!(matches!(to_logical(Camera), Key::Camera));
  }

  #[test]
  fn to_logical_explorer_envelope() {
    assert!(matches!(to_logical(Explorer), Key::LaunchWebBrowser));
    assert!(matches!(to_logical(Envelope), Key::LaunchMail));
  }

  #[test]
  fn to_logical_sym_maps_to_symbol() {
    assert!(matches!(to_logical(Sym), Key::Symbol));
  }

  // ─── to_logical: clipboard & refresh ──────────────────────────────────

  #[test]
  fn to_logical_cut_copy_paste() {
    assert!(matches!(to_logical(Cut), Key::Cut));
    assert!(matches!(to_logical(Copy), Key::Copy));
    assert!(matches!(to_logical(Paste), Key::Paste));
  }

  #[test]
  fn to_logical_refresh_maps_to_browser_refresh() {
    assert!(matches!(to_logical(Refresh), Key::BrowserRefresh));
  }

  // ─── to_logical: TV & brightness ─────────────────────────────────────

  #[test]
  fn to_logical_tv_keys() {
    assert!(matches!(to_logical(TV), Key::TV));
    assert!(matches!(to_logical(ChannelUp), Key::ChannelUp));
    assert!(matches!(to_logical(ChannelDown), Key::ChannelDown));
  }

  #[test]
  fn to_logical_zoom_keys() {
    assert!(matches!(to_logical(ZoomIn), Key::ZoomIn));
    assert!(matches!(to_logical(ZoomOut), Key::ZoomOut));
  }

  #[test]
  fn to_logical_brightness_keys() {
    assert!(matches!(to_logical(BrightnessDown), Key::BrightnessDown));
    assert!(matches!(to_logical(BrightnessUp), Key::BrightnessUp));
  }

  #[test]
  fn to_logical_info() {
    assert!(matches!(to_logical(Info), Key::Info));
  }

  // ─── to_logical: Japanese keys ───────────────────────────────────────

  #[test]
  fn to_logical_japanese_keys() {
    assert!(matches!(to_logical(ZenkakuHankaku), Key::ZenkakuHankaku));
    assert!(matches!(to_logical(Muhenkan), Key::NonConvert));
    assert!(matches!(to_logical(Henkan), Key::Convert));
    assert!(matches!(to_logical(KatakanaHiragana), Key::HiraganaKatakana));
  }

  // ─── to_logical: calendar & sleep ────────────────────────────────────

  #[test]
  fn to_logical_calendar_maps_to_launch_calendar() {
    assert!(matches!(to_logical(Calendar), Key::LaunchCalendar));
  }

  #[test]
  fn to_logical_sleep_wakeup() {
    assert!(matches!(to_logical(Sleep), Key::Standby));
    assert!(matches!(to_logical(Wakeup), Key::WakeUp));
  }

  #[test]
  fn to_logical_help() {
    assert!(matches!(to_logical(Help), Key::Help));
  }

  // ─── to_logical: unidentified fallbacks ────────────────────────────────

  #[test]
  fn to_logical_unknown_maps_to_unidentified() {
    let result = to_logical(Unknown);
    assert!(matches!(result, Key::Unidentified(_)));
  }

  #[test]
  fn to_logical_numpad_keys_are_unidentified() {
    // Numpad digits and operators map to Unidentified (no web numpad mapping)
    for kc in [
      Numpad0, Numpad1, Numpad2, Numpad3, Numpad4,
      Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    ] {
      assert!(matches!(to_logical(kc), Key::Unidentified(_)), "numpad digit should be Unidentified");
    }
    assert!(matches!(to_logical(NumpadDivide), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadMultiply), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadSubtract), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadAdd), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadDot), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadComma), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadEnter), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadEquals), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadLeftParen), Key::Unidentified(_)));
    assert!(matches!(to_logical(NumpadRightParen), Key::Unidentified(_)));
  }

  #[test]
  fn to_logical_alpha_keys_are_unidentified_fallback() {
    // A-Z and 0-9 keys are matched as Unidentified (they should be resolved
    // via KeyMapChar::Unicode first, but this is a fallback)
    assert!(matches!(to_logical(A), Key::Unidentified(_)));
    assert!(matches!(to_logical(Z), Key::Unidentified(_)));
    assert!(matches!(to_logical(Key0), Key::Unidentified(_)));
    assert!(matches!(to_logical(Key9), Key::Unidentified(_)));
  }

  #[test]
  fn to_logical_punctuation_keys_are_unidentified_fallback() {
    assert!(matches!(to_logical(Comma), Key::Unidentified(_)));
    assert!(matches!(to_logical(Period), Key::Unidentified(_)));
    assert!(matches!(to_logical(Grave), Key::Unidentified(_)));
    assert!(matches!(to_logical(Minus), Key::Unidentified(_)));
    assert!(matches!(to_logical(Equals), Key::Unidentified(_)));
    assert!(matches!(to_logical(LeftBracket), Key::Unidentified(_)));
    assert!(matches!(to_logical(RightBracket), Key::Unidentified(_)));
    assert!(matches!(to_logical(Backslash), Key::Unidentified(_)));
    assert!(matches!(to_logical(Semicolon), Key::Unidentified(_)));
    assert!(matches!(to_logical(Apostrophe), Key::Unidentified(_)));
    assert!(matches!(to_logical(Slash), Key::Unidentified(_)));
    assert!(matches!(to_logical(At), Key::Unidentified(_)));
    assert!(matches!(to_logical(Plus), Key::Unidentified(_)));
  }

  #[test]
  fn to_logical_yen_ro_are_unidentified() {
    assert!(matches!(to_logical(Yen), Key::Unidentified(_)));
    assert!(matches!(to_logical(Ro), Key::Unidentified(_)));
  }

  #[test]
  fn to_logical_menu_is_unidentified() {
    assert!(matches!(to_logical(Menu), Key::Unidentified(_)));
  }

  // ─── to_location: modifier location ───────────────────────────────────

  #[test]
  fn to_location_left_modifiers() {
    assert_eq!(to_location(AltLeft), KeyLocation::Left);
    assert_eq!(to_location(ShiftLeft), KeyLocation::Left);
    assert_eq!(to_location(CtrlLeft), KeyLocation::Left);
    assert_eq!(to_location(MetaLeft), KeyLocation::Left);
  }

  #[test]
  fn to_location_right_modifiers() {
    assert_eq!(to_location(AltRight), KeyLocation::Right);
    assert_eq!(to_location(ShiftRight), KeyLocation::Right);
    assert_eq!(to_location(CtrlRight), KeyLocation::Right);
    assert_eq!(to_location(MetaRight), KeyLocation::Right);
  }

  #[test]
  fn to_location_numpad_keys() {
    for kc in [
      NumLock, Numpad0, Numpad1, Numpad2, Numpad3, Numpad4,
      Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
      NumpadDivide, NumpadMultiply, NumpadSubtract, NumpadAdd,
      NumpadDot, NumpadComma, NumpadEnter, NumpadEquals,
      NumpadLeftParen, NumpadRightParen,
    ] {
      assert_eq!(to_location(kc), KeyLocation::Numpad, "numpad key should be Numpad location");
    }
  }

  #[test]
  fn to_location_non_modifier_non_numpad_is_standard() {
    // Regular keys (letters, digits, F-keys, arrows, etc.) are Standard
    for kc in [
      A, B, Key0, Key9, F1, F12, DpadUp, DpadDown,
      Space, Tab, Enter, Escape, VolumeUp, Home, PageUp,
    ] {
      assert_eq!(to_location(kc), KeyLocation::Standard, "regular key should be Standard location");
    }
  }
}
