use cosmic_settings_config::shortcuts::State as KeyState;
use cosmic_settings_config::shortcuts::{self, Modifiers};
use smithay::input::keyboard::ModifiersState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Behaviors managed internally by cosmic-comp.
    Private(PrivateAction),
    /// Behaviors managed via cosmic-settings.
    Shortcut(shortcuts::Action),
}

/// Zone actions triggered by our own bindings.
///
/// These deliberately do not go through `cosmic_settings_config::shortcuts::Action`:
/// that enum lives in an external repository, so extending it would mean
/// maintaining a second fork. They are matched ahead of the standard shortcut
/// table instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneAction {
    /// Move the focused window to the next zone in the layout.
    CycleNext,
    /// Move the focused window to the previous zone.
    CyclePrev,
    /// Extend the focused window's span by one adjacent zone.
    GrowSpan,
    /// Reduce the focused window's span by one zone.
    ShrinkSpan,
    /// Launch the zone editor.
    OpenEditor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Behaviors which are internally defined and emitted.
pub enum PrivateAction {
    Escape,
    Zone(ZoneAction),
    Resizing(
        shortcuts::action::ResizeDirection,
        shortcuts::action::ResizeEdge,
        shortcuts::State,
    ),
}

/// Convert `cosmic_settings_config::shortcuts::State` to `smithay::backend::input::KeyState`.
pub fn cosmic_keystate_to_smithay(value: KeyState) -> smithay::backend::input::KeyState {
    match value {
        KeyState::Pressed => smithay::backend::input::KeyState::Pressed,
        KeyState::Released => smithay::backend::input::KeyState::Released,
    }
}

/// Convert `smithay::backend::input::KeyState` to `cosmic_settings_config::shortcuts::State`.
pub fn cosmic_keystate_from_smithay(value: smithay::backend::input::KeyState) -> KeyState {
    match value {
        smithay::backend::input::KeyState::Pressed => KeyState::Pressed,
        smithay::backend::input::KeyState::Released => KeyState::Released,
    }
}

/// Compare `cosmic_settings_config::shortcuts::Modifiers` to `smithay::input::keyboard::ModifiersState`.
pub fn cosmic_modifiers_eq_smithay(this: &Modifiers, other: &ModifiersState) -> bool {
    this.ctrl == other.ctrl
        && this.alt == other.alt
        && this.shift == other.shift
        && this.logo == other.logo
}

/// Convert `smithay::input::keyboard::ModifiersState` to `cosmic_settings_config::shortcuts::Modifiers`
pub fn cosmic_modifiers_from_smithay(value: ModifiersState) -> Modifiers {
    Modifiers {
        ctrl: value.ctrl,
        alt: value.alt,
        shift: value.shift,
        logo: value.logo,
    }
}
