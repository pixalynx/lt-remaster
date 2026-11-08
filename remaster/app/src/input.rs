//! The pad: keyboard keys (the Play screen's defaults, `app/src/screens/22-play/input.ts`, or the
//! player's own from Living Tool's settings with `--keys`) and any game controller (gilrs, the
//! standard layout), ORed together into the PS1's button bits. In `--host` mode Living Tool reads
//! the controllers and sends their buttons with the pad, so the window reads keys only.

use std::collections::{HashMap, HashSet};

use gilrs::{Axis, Button, Gilrs};
use winit::keyboard::KeyCode;

pub const SELECT: u16 = 1 << 0;
pub const START: u16 = 1 << 3;
pub const UP: u16 = 1 << 4;
pub const RIGHT: u16 = 1 << 5;
pub const DOWN: u16 = 1 << 6;
pub const LEFT: u16 = 1 << 7;
pub const L2: u16 = 1 << 8;
pub const R2: u16 = 1 << 9;
pub const L1: u16 = 1 << 10;
pub const R1: u16 = 1 << 11;
pub const TRIANGLE: u16 = 1 << 12;
pub const CIRCLE: u16 = 1 << 13;
pub const CROSS: u16 = 1 << 14;
pub const SQUARE: u16 = 1 << 15;

/// The Play screen's default keys.
pub fn key_bits(code: KeyCode) -> u16 {
    match code {
        KeyCode::ArrowUp => UP,
        KeyCode::ArrowDown => DOWN,
        KeyCode::ArrowLeft => LEFT,
        KeyCode::ArrowRight => RIGHT,
        KeyCode::KeyX => CROSS,
        KeyCode::KeyC => CIRCLE,
        KeyCode::KeyZ => SQUARE,
        KeyCode::KeyS => TRIANGLE,
        KeyCode::KeyQ => L1,
        KeyCode::KeyW => R1,
        KeyCode::Digit1 => L2,
        KeyCode::Digit2 => R2,
        KeyCode::Enter => START,
        KeyCode::Backspace => SELECT,
        _ => 0,
    }
}

/// The PS1 buttons by the names Living Tool's settings use (`inproc.keys`, `DEFAULT_KEYS`).
const BUTTONS: [(&str, u16); 14] = [
    ("up", UP),
    ("down", DOWN),
    ("left", LEFT),
    ("right", RIGHT),
    ("cross", CROSS),
    ("circle", CIRCLE),
    ("square", SQUARE),
    ("triangle", TRIANGLE),
    ("l1", L1),
    ("r1", R1),
    ("l2", L2),
    ("r2", R2),
    ("start", START),
    ("select", SELECT),
];

macro_rules! codes {
    ($($name:ident)*) => {
        /// A key by its `KeyboardEvent.code` name (the key's place, whatever the layout), which
        /// is winit's `KeyCode` name too.
        pub fn code_named(name: &str) -> Option<KeyCode> {
            match name {
                $(stringify!($name) => Some(KeyCode::$name),)*
                // The browser's names for the ⌘ / Windows keys.
                "MetaLeft" => Some(KeyCode::SuperLeft),
                "MetaRight" => Some(KeyCode::SuperRight),
                _ => None,
            }
        }
    };
}

codes! {
    KeyA KeyB KeyC KeyD KeyE KeyF KeyG KeyH KeyI KeyJ KeyK KeyL KeyM KeyN KeyO KeyP KeyQ KeyR KeyS
    KeyT KeyU KeyV KeyW KeyX KeyY KeyZ Digit0 Digit1 Digit2 Digit3 Digit4 Digit5 Digit6 Digit7 Digit8
    Digit9 ArrowUp ArrowDown ArrowLeft ArrowRight Enter Backspace Space Tab Escape ShiftLeft ShiftRight
    ControlLeft ControlRight AltLeft AltRight Numpad0 Numpad1 Numpad2 Numpad3
    Numpad4 Numpad5 Numpad6 Numpad7 Numpad8 Numpad9 NumpadEnter NumpadAdd NumpadSubtract
    NumpadMultiply NumpadDivide NumpadDecimal Comma Period Slash Semicolon Quote BracketLeft
    BracketRight Backslash Minus Equal Backquote F1 F2 F4 F5 F6 F7 F8 F9 F10 F11 F12 Home End PageUp
    PageDown Insert Delete
}

/// `--keys`: `button=Code` pairs, comma-separated (`cross=KeyX,start=Enter`); a button not named
/// keeps its default key. Unknown buttons or keys are an error.
pub fn parse_keys(s: &str) -> Result<HashMap<KeyCode, u16>, String> {
    let mut by_button: HashMap<u16, KeyCode> = HashMap::new();
    for (name, bit) in BUTTONS {
        let default = [
            KeyCode::ArrowUp, KeyCode::ArrowDown, KeyCode::ArrowLeft, KeyCode::ArrowRight, KeyCode::KeyX, KeyCode::KeyC, KeyCode::KeyZ,
            KeyCode::KeyS, KeyCode::KeyQ, KeyCode::KeyW, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Enter, KeyCode::Backspace,
        ][BUTTONS.iter().position(|(n, _)| *n == name).expect("listed")];
        by_button.insert(bit, default);
    }
    for pair in s.split(',').filter(|p| !p.trim().is_empty()) {
        let (b, k) = pair.split_once('=').ok_or_else(|| format!("--keys: {pair} isn't button=Code"))?;
        let bit = BUTTONS.iter().find(|(n, _)| *n == b.trim()).map(|(_, bit)| *bit).ok_or_else(|| format!("--keys: no PS1 button {b}"))?;
        let code = code_named(k.trim()).ok_or_else(|| format!("--keys: no key {k}"))?;
        by_button.insert(bit, code);
    }
    let mut map: HashMap<KeyCode, u16> = HashMap::new();
    for (bit, code) in by_button {
        *map.entry(code).or_default() |= bit;
    }
    Ok(map)
}

/// The standard controller layout (`lt_emu::gamepad`'s default mapping).
fn button_bits(b: Button) -> u16 {
    match b {
        Button::South => CROSS,
        Button::East => CIRCLE,
        Button::West => SQUARE,
        Button::North => TRIANGLE,
        Button::LeftTrigger => L1,
        Button::RightTrigger => R1,
        Button::LeftTrigger2 => L2,
        Button::RightTrigger2 => R2,
        Button::Start => START,
        Button::Select => SELECT,
        Button::DPadUp => UP,
        Button::DPadDown => DOWN,
        Button::DPadLeft => LEFT,
        Button::DPadRight => RIGHT,
        _ => 0,
    }
}

/// How far a stick goes before it counts as a direction.
const STICK: f32 = 0.5;

pub struct Input {
    keys: HashSet<KeyCode>,
    /// The player's keys (`--keys`); `None`: the Play screen's defaults.
    map: Option<HashMap<KeyCode, u16>>,
    gilrs: Option<Gilrs>,
}

impl Input {
    /// With `controllers` it reads game controllers too.
    pub fn new(controllers: bool, map: Option<HashMap<KeyCode, u16>>) -> Self {
        Input { keys: HashSet::new(), map, gilrs: if controllers { Gilrs::new().ok() } else { None } }
    }

    /// The PS1 buttons `code` holds.
    pub fn bits(&self, code: KeyCode) -> u16 {
        match &self.map {
            Some(m) => m.get(&code).copied().unwrap_or(0),
            None => key_bits(code),
        }
    }

    pub fn key(&mut self, code: KeyCode, down: bool) {
        if down {
            self.keys.insert(code);
        } else {
            self.keys.remove(&code);
        }
    }

    /// Forget held keys (the window lost focus).
    pub fn release_keys(&mut self) {
        self.keys.clear();
    }

    /// The pad now: held keys, then every connected controller's buttons and left stick.
    pub fn pad(&mut self) -> u16 {
        let mut bits = self.keys.iter().fold(0, |b, &k| b | self.bits(k));
        if let Some(g) = self.gilrs.as_mut() {
            while g.next_event().is_some() {}
            for (_, pad) in g.gamepads() {
                for b in [
                    Button::South, Button::East, Button::West, Button::North, Button::LeftTrigger, Button::RightTrigger,
                    Button::LeftTrigger2, Button::RightTrigger2, Button::Start, Button::Select, Button::DPadUp, Button::DPadDown,
                    Button::DPadLeft, Button::DPadRight,
                ] {
                    if pad.is_pressed(b) {
                        bits |= button_bits(b);
                    }
                }
                let (x, y) = (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY));
                if x < -STICK {
                    bits |= LEFT;
                }
                if x > STICK {
                    bits |= RIGHT;
                }
                if y > STICK {
                    bits |= UP;
                }
                if y < -STICK {
                    bits |= DOWN;
                }
            }
        }
        bits
    }

    pub fn controllers(&self) -> usize {
        self.gilrs.as_ref().map_or(0, |g| g.gamepads().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_play_screen_keys() {
        assert_eq!(key_bits(KeyCode::KeyX), CROSS);
        assert_eq!(key_bits(KeyCode::Enter), START);
        assert_eq!(key_bits(KeyCode::ArrowUp), UP);
        assert_eq!(key_bits(KeyCode::KeyA), 0);
    }

    #[test]
    fn the_players_own_keys() {
        let map = parse_keys("cross=KeyJ,start=Space").unwrap();
        let i = Input::new(false, Some(map));
        assert_eq!((i.bits(KeyCode::KeyJ), i.bits(KeyCode::Space), i.bits(KeyCode::KeyX)), (CROSS, START, 0));
        assert_eq!(i.bits(KeyCode::ArrowUp), UP, "the rest keep their defaults");
        assert_eq!(code_named("Digit2"), Some(KeyCode::Digit2));
        assert!(parse_keys("jump=KeyJ").is_err() && parse_keys("cross=Nope").is_err());
    }

    #[test]
    fn held_keys_or_together() {
        let mut i = Input { keys: HashSet::new(), map: None, gilrs: None };
        i.key(KeyCode::ArrowUp, true);
        i.key(KeyCode::KeyX, true);
        assert_eq!(i.pad(), UP | CROSS);
        i.key(KeyCode::ArrowUp, false);
        assert_eq!(i.pad(), CROSS);
        i.release_keys();
        assert_eq!(i.pad(), 0);
    }
}
