//! Player controls, configured in the viewer instead of the game's own Configure Controls dialog.
//!
//! The game keeps its default keyboard controls (arrows, Shift, Ctrl, Backspace, Space, Enter).
//! The viewer maps what the player presses onto those keys:
//!   - keyboard: each game action can be bound to any key; unbound game-control keys are
//!     swallowed so a rebound action doesn't also fire from its old key, and all other keys
//!     (F1 help, F2 reset, ...) pass through unchanged;
//!   - controller: an XInput pad (8BitDo pads in their default X-input mode, Xbox pads, ...)
//!     is read every frame, and each button / direction can be set to any action.
//! Settings are saved in controls.cfg next to the viewer.

use std::collections::BTreeSet;
use std::path::PathBuf;

use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::XboxController::*;

/// Game actions: (menu label, short name used in controls.cfg, the game's default key).
pub const ACTIONS: [(&str, &str, VIRTUAL_KEY); 11] = [
    ("Nothing", "none", VIRTUAL_KEY(0)),
    ("Left", "left", VK_LEFT),
    ("Right", "right", VK_RIGHT),
    ("Up (rotate aura / enter)", "up", VK_UP),
    ("Down (rotate aura / enter)", "down", VK_DOWN),
    ("Button 1 (jump / confirm)", "button1", VK_SHIFT),
    ("Button 2 (capture / throw / cancel)", "button2", VK_CONTROL),
    ("Button 3 (exit beaten level)", "button3", VK_BACK),
    ("Button 4", "button4", VK_SPACE),
    ("Pause", "pause", VK_RETURN),
    ("Reset game", "reset", VK_F2),
];

/// Actions that can be bound to a keyboard key (Reset stays on F2 / the Game menu).
pub const KEYBOARD_ACTIONS: std::ops::RangeInclusive<usize> = 1..=9;

/// Pad inputs, in menu order.
pub const INPUTS: [&str; 20] = [
    "D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right",
    "Stick Up", "Stick Down", "Stick Left", "Stick Right",
    "A", "B", "X", "Y", "LB", "RB", "LT", "RT", "Select / Back", "Start",
    "Left stick click", "Right stick click",
];

/// Default action (index into ACTIONS) for each pad input.
const DEFAULT_PAD: [usize; 20] = [
    3, 4, 1, 2, // D-pad
    3, 4, 1, 2, // stick
    5, 6, 6, 8, // A jump, B and X capture/throw, Y button 4
    0, 0, 0, 0, // shoulders and triggers unused
    7, 9, // Select = Button 3, Start = pause
    0, 0,
];

const STICK_DEADZONE: i16 = 16000;
const TRIGGER_THRESHOLD: u8 = 128;

/// What to do with a key the player pressed in the viewer.
#[derive(Debug, PartialEq)]
pub enum KeyRoute {
    /// Send the game this key instead.
    Send(u16),
    /// A game-control key no action is bound to: drop it.
    Swallow,
    /// Not a control key: pass it through unchanged.
    Pass,
}

pub struct Controls {
    pub pad_enabled: bool,
    /// Action for each of INPUTS.
    pub pad_map: [usize; 20],
    /// Keyboard key (virtual key, 0 = none) bound to each of ACTIONS.
    pub key_map: [u16; 11],
    path: PathBuf,
    pad: Option<u32>,
    pub pad_name: String,
    /// Game keys currently held down because of the pad.
    held: BTreeSet<u16>,
    polls_since_scan: u32,
}

fn default_keys() -> [u16; 11] {
    let mut k = [0u16; 11];
    for a in KEYBOARD_ACTIONS {
        k[a] = ACTIONS[a].2 .0;
    }
    k
}

impl Controls {
    pub fn load(path: PathBuf) -> Controls {
        let mut c = Controls {
            pad_enabled: true,
            pad_map: DEFAULT_PAD,
            key_map: default_keys(),
            path,
            pad: None,
            pad_name: "No controller found".into(),
            held: BTreeSet::new(),
            polls_since_scan: u32::MAX,
        };
        let action = |short: &str| ACTIONS.iter().position(|a| a.1 == short);
        if let Ok(text) = std::fs::read_to_string(&c.path) {
            for line in text.lines() {
                let Some((k, v)) = line.split_once('=') else { continue };
                let (k, v) = (k.trim(), v.trim());
                if k == "pad_enabled" {
                    c.pad_enabled = v == "1";
                } else if let Some(name) = k.strip_prefix("key.") {
                    if let (Some(a), Ok(vk)) = (action(name), v.parse::<u16>()) {
                        if KEYBOARD_ACTIONS.contains(&a) {
                            c.key_map[a] = vk;
                        }
                    }
                } else if let Some(name) = k.strip_prefix("pad.") {
                    if let (Some(i), Some(a)) = (INPUTS.iter().position(|n| *n == name), action(v)) {
                        c.pad_map[i] = a;
                    }
                }
            }
        }
        c
    }

    pub fn save(&self) {
        let mut text = format!("pad_enabled={}\n", self.pad_enabled as u8);
        for a in KEYBOARD_ACTIONS {
            text += &format!("key.{}={}\n", ACTIONS[a].1, self.key_map[a]);
        }
        for (i, name) in INPUTS.iter().enumerate() {
            text += &format!("pad.{name}={}\n", ACTIONS[self.pad_map[i]].1);
        }
        let _ = std::fs::write(&self.path, text);
    }

    pub fn reset_keys(&mut self) {
        self.key_map = default_keys();
        self.save();
    }

    pub fn reset_pad(&mut self) {
        self.pad_map = DEFAULT_PAD;
        self.save();
    }

    /// Binds `vk` to `action`, taking it away from any other action.
    pub fn bind_key(&mut self, action: usize, vk: u16) {
        for k in self.key_map.iter_mut() {
            if *k == vk {
                *k = 0;
            }
        }
        self.key_map[action] = vk;
        self.save();
    }

    pub fn route_key(&self, vk: u16) -> KeyRoute {
        if vk == 0 {
            return KeyRoute::Pass;
        }
        if let Some(a) = self.key_map.iter().position(|&k| k == vk) {
            return KeyRoute::Send(ACTIONS[a].2 .0);
        }
        if KEYBOARD_ACTIONS.clone().any(|a| ACTIONS[a].2 .0 == vk) {
            return KeyRoute::Swallow;
        }
        KeyRoute::Pass
    }

    /// Reads the pad and returns the game key presses and releases since the last poll, as
    /// (virtual key, down). Pass `active = false` (viewer not focused) to release everything.
    pub fn poll(&mut self, active: bool) -> Vec<(u16, bool)> {
        let mut want = BTreeSet::new();
        if active && self.pad_enabled {
            if let Some(state) = self.read() {
                want = self.keys_for(&state.Gamepad);
            }
        }
        let mut events: Vec<(u16, bool)> = self.held.difference(&want).map(|&k| (k, false)).collect();
        events.extend(want.difference(&self.held).map(|&k| (k, true)));
        self.held = want;
        events
    }

    /// Game keys the pad state asks to hold down under the current mapping.
    fn keys_for(&self, pad: &XINPUT_GAMEPAD) -> BTreeSet<u16> {
        pressed_inputs(pad).iter().enumerate()
            .filter(|(_, on)| **on)
            .map(|(i, _)| ACTIONS[self.pad_map[i]].2 .0)
            .filter(|&vk| vk != 0)
            .collect()
    }

    /// State of the connected pad, looking for one (about once a second) when there is none.
    fn read(&mut self) -> Option<XINPUT_STATE> {
        let mut state = XINPUT_STATE::default();
        if let Some(i) = self.pad {
            if unsafe { XInputGetState(i, &mut state) } == 0 {
                return Some(state);
            }
            self.pad = None;
            self.pad_name = "No controller found".into();
        }
        self.polls_since_scan = self.polls_since_scan.saturating_add(1);
        if self.polls_since_scan < 60 {
            return None;
        }
        self.polls_since_scan = 0;
        for i in 0..XUSER_MAX_COUNT {
            if unsafe { XInputGetState(i, &mut state) } == 0 {
                self.pad = Some(i);
                self.pad_name = format!("Controller {} connected", i + 1);
                return Some(state);
            }
        }
        None
    }
}

fn pressed_inputs(g: &XINPUT_GAMEPAD) -> [bool; 20] {
    let b = |m: XINPUT_GAMEPAD_BUTTON_FLAGS| g.wButtons.0 & m.0 != 0;
    [
        b(XINPUT_GAMEPAD_DPAD_UP), b(XINPUT_GAMEPAD_DPAD_DOWN), b(XINPUT_GAMEPAD_DPAD_LEFT), b(XINPUT_GAMEPAD_DPAD_RIGHT),
        g.sThumbLY > STICK_DEADZONE, g.sThumbLY < -STICK_DEADZONE,
        g.sThumbLX < -STICK_DEADZONE, g.sThumbLX > STICK_DEADZONE,
        b(XINPUT_GAMEPAD_A), b(XINPUT_GAMEPAD_B), b(XINPUT_GAMEPAD_X), b(XINPUT_GAMEPAD_Y),
        b(XINPUT_GAMEPAD_LEFT_SHOULDER), b(XINPUT_GAMEPAD_RIGHT_SHOULDER),
        g.bLeftTrigger >= TRIGGER_THRESHOLD, g.bRightTrigger >= TRIGGER_THRESHOLD,
        b(XINPUT_GAMEPAD_BACK), b(XINPUT_GAMEPAD_START),
        b(XINPUT_GAMEPAD_LEFT_THUMB), b(XINPUT_GAMEPAD_RIGHT_THUMB),
    ]
}

fn is_extended(vk: u16) -> bool {
    [VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_INSERT, VK_DELETE, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_RCONTROL, VK_RMENU]
        .iter().any(|k| k.0 == vk)
}

/// lParam for a synthesized WM_KEYDOWN / WM_KEYUP of `vk`.
pub fn key_lparam(vk: u16, down: bool) -> isize {
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as isize;
    let mut l = 1 | (scan << 16) | if is_extended(vk) { 1 << 24 } else { 0 };
    if !down {
        l |= 0xC000_0000u32 as isize; // previous state down, transition up
    }
    l
}

/// Display name of a key ("Left", "Shift", "Z", ...); "(none)" for 0.
pub fn key_name(vk: u16) -> String {
    if vk == 0 {
        return "(none)".into();
    }
    let mut buf = [0u16; 64];
    let n = unsafe { GetKeyNameTextW(key_lparam(vk, true) as i32, &mut buf) };
    if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { format!("key {vk}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(buttons: XINPUT_GAMEPAD_BUTTON_FLAGS, lx: i16, ly: i16) -> XINPUT_GAMEPAD {
        XINPUT_GAMEPAD { wButtons: buttons, sThumbLX: lx, sThumbLY: ly, ..Default::default() }
    }

    fn controls() -> Controls {
        Controls::load(std::env::temp_dir().join("hd_view_controls_test_missing.cfg"))
    }

    #[test]
    fn default_pad_mapping_matches_game_controls() {
        let c = controls();
        let keys = c.keys_for(&pad(XINPUT_GAMEPAD_A | XINPUT_GAMEPAD_DPAD_RIGHT | XINPUT_GAMEPAD_START, 0, 0));
        assert_eq!(keys, [VK_SHIFT.0, VK_RIGHT.0, VK_RETURN.0].into_iter().collect());
        assert_eq!(c.keys_for(&pad(XINPUT_GAMEPAD_B, 0, 0)), [VK_CONTROL.0].into_iter().collect());
        assert_eq!(c.keys_for(&pad(XINPUT_GAMEPAD_BACK, 0, 0)), [VK_BACK.0].into_iter().collect());
    }

    #[test]
    fn stick_uses_deadzone() {
        let c = controls();
        assert!(c.keys_for(&pad(XINPUT_GAMEPAD_BUTTON_FLAGS(0), 8000, -8000)).is_empty());
        assert_eq!(c.keys_for(&pad(XINPUT_GAMEPAD_BUTTON_FLAGS(0), -30000, 30000)),
                   [VK_LEFT.0, VK_UP.0].into_iter().collect());
    }

    #[test]
    fn pad_remap_and_unmap() {
        let mut c = controls();
        c.pad_map[8] = 0; // A -> nothing
        c.pad_map[12] = 5; // LB -> button 1
        assert!(c.keys_for(&pad(XINPUT_GAMEPAD_A, 0, 0)).is_empty());
        assert_eq!(c.keys_for(&pad(XINPUT_GAMEPAD_LEFT_SHOULDER, 0, 0)), [VK_SHIFT.0].into_iter().collect());
    }

    #[test]
    fn keyboard_defaults_pass_game_keys_through() {
        let c = controls();
        assert_eq!(c.route_key(VK_LEFT.0), KeyRoute::Send(VK_LEFT.0));
        assert_eq!(c.route_key(VK_SHIFT.0), KeyRoute::Send(VK_SHIFT.0));
        assert_eq!(c.route_key(VK_F1.0), KeyRoute::Pass);
        assert_eq!(c.route_key(0x5A), KeyRoute::Pass); // Z unbound and not a game key
    }

    #[test]
    fn keyboard_rebind_moves_the_key() {
        let mut c = controls();
        c.path = std::env::temp_dir().join("hd_view_controls_test_rebind.cfg");
        c.bind_key(5, 0x5A); // Button 1 (jump) -> Z
        assert_eq!(c.route_key(0x5A), KeyRoute::Send(VK_SHIFT.0));
        assert_eq!(c.route_key(VK_SHIFT.0), KeyRoute::Swallow); // old jump key no longer jumps
        c.bind_key(6, 0x5A); // Z taken by Button 2 instead
        assert_eq!(c.key_map[5], 0);
        assert_eq!(c.route_key(0x5A), KeyRoute::Send(VK_CONTROL.0));
        let _ = std::fs::remove_file(&c.path);
    }

    #[test]
    fn settings_round_trip() {
        let path = std::env::temp_dir().join("hd_view_controls_test.cfg");
        let mut c = Controls::load(path.clone());
        c.pad_enabled = false;
        c.pad_map[9] = 9; // B -> pause
        c.bind_key(5, 0x5A);
        let d = Controls::load(path.clone());
        let _ = std::fs::remove_file(path);
        assert!(!d.pad_enabled);
        assert_eq!(d.pad_map[9], 9);
        assert_eq!(d.pad_map[8], DEFAULT_PAD[8]);
        assert_eq!(d.key_map[5], 0x5A);
        assert_eq!(d.key_map[1], VK_LEFT.0);
    }
}
