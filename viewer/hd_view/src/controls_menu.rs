//! The viewer's Controls menu (keyboard bindings and controller mapping) and the routing of
//! the player's input to the game. Replaces the game's own Configure Controls dialog, which is
//! removed from the copied menu. UI-thread state lives in thread-locals.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::controls::{self, Controls, KeyRoute, ACTIONS, INPUTS, KEYBOARD_ACTIONS};

/// The game's Game > Configure Controls command (MMF2 runtime id); the viewer's menu replaces it.
pub const CMD_CONFIGURE_CONTROLS: u32 = 1013;

// Viewer-owned menu command ids (the game's own are all below 40000).
const ID_KEY_RESET: u32 = 40000;
const ID_PAD_ENABLE: u32 = 40001;
const ID_PAD_STATUS: u32 = 40002;
const ID_PAD_RESET: u32 = 40003;
/// Keyboard binding items: ID_KEY_BIND + action.
const ID_KEY_BIND: u32 = 40010;
/// Controller mapping items: ID_PAD_MAP + input * 32 + action.
const ID_PAD_MAP: u32 = 40100;

struct Menus {
    keyboard: HMENU,
    pad: HMENU,
    pad_inputs: Vec<HMENU>,
}

thread_local! {
    static CONTROLS: RefCell<Option<Controls>> = const { RefCell::new(None) };
    static MENUS: RefCell<Option<Menus>> = const { RefCell::new(None) };
    /// Action waiting for the player to press its new key.
    static CAPTURING: Cell<Option<usize>> = const { Cell::new(None) };
    /// Whether the last key press went to the game unchanged (so its WM_CHAR may follow).
    static LAST_KEY_PASSED: Cell<bool> = const { Cell::new(true) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn short(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label)
}

pub fn init(path: PathBuf) {
    CONTROLS.with(|c| *c.borrow_mut() = Some(Controls::load(path)));
}

fn with<R>(f: impl FnOnce(&mut Controls) -> R) -> Option<R> {
    CONTROLS.with(|c| c.borrow_mut().as_mut().map(f))
}

/// Appends a fresh Controls popup (Keyboard, Controller) to `bar`.
pub unsafe fn add_to(bar: HMENU) {
    let Some(menus) = with(|c| build(c)) else { return };
    let popup = CreatePopupMenu().unwrap();
    let _ = AppendMenuW(popup, MF_POPUP, menus.keyboard.0 as usize, PCWSTR(wide("&Keyboard").as_ptr()));
    let _ = AppendMenuW(popup, MF_POPUP, menus.pad.0 as usize, PCWSTR(wide("&Controller").as_ptr()));
    let _ = AppendMenuW(bar, MF_POPUP, popup.0 as usize, PCWSTR(wide("&Controls").as_ptr()));
    MENUS.with(|m| *m.borrow_mut() = Some(menus));
}

unsafe fn build(c: &Controls) -> Menus {
    let keyboard = CreatePopupMenu().unwrap();
    for a in KEYBOARD_ACTIONS {
        let _ = AppendMenuW(keyboard, MF_STRING, (ID_KEY_BIND + a as u32) as usize, PCWSTR(wide(&key_label(c, a)).as_ptr()));
    }
    let _ = AppendMenuW(keyboard, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(keyboard, MF_STRING, ID_KEY_RESET as usize, PCWSTR(wide("Reset keyboard to defaults").as_ptr()));

    let pad = CreatePopupMenu().unwrap();
    let _ = AppendMenuW(pad, MF_STRING, ID_PAD_ENABLE as usize, PCWSTR(wide("Use controller").as_ptr()));
    let _ = AppendMenuW(pad, MF_STRING | MF_GRAYED, ID_PAD_STATUS as usize, PCWSTR(wide(&c.pad_name).as_ptr()));
    let _ = AppendMenuW(pad, MF_SEPARATOR, 0, PCWSTR::null());
    let mut pad_inputs = Vec::new();
    for (i, _) in INPUTS.iter().enumerate() {
        let sub = CreatePopupMenu().unwrap();
        for (a, (name, _, _)) in ACTIONS.iter().enumerate() {
            let _ = AppendMenuW(sub, MF_STRING, (ID_PAD_MAP + i as u32 * 32 + a as u32) as usize, PCWSTR(wide(name).as_ptr()));
        }
        let _ = AppendMenuW(pad, MF_POPUP, sub.0 as usize, PCWSTR(wide(&pad_label(c, i)).as_ptr()));
        pad_inputs.push(sub);
    }
    let _ = AppendMenuW(pad, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(pad, MF_STRING, ID_PAD_RESET as usize, PCWSTR(wide("Reset controller to defaults").as_ptr()));
    Menus { keyboard, pad, pad_inputs }
}

fn key_label(c: &Controls, action: usize) -> String {
    format!("{}:\t{}", ACTIONS[action].0, controls::key_name(c.key_map[action]))
}

fn pad_label(c: &Controls, input: usize) -> String {
    format!("{}:\t{}", INPUTS[input], short(ACTIONS[c.pad_map[input]].0))
}

/// Refreshes labels, check marks and the controller status before one of our menus opens.
pub unsafe fn on_init_popup(opening: HMENU) {
    MENUS.with(|m| {
        let m = m.borrow();
        let Some(m) = m.as_ref() else { return };
        let _ = with(|c| {
            if opening == m.keyboard {
                for a in KEYBOARD_ACTIONS {
                    let id = ID_KEY_BIND + a as u32;
                    let _ = ModifyMenuW(m.keyboard, id, MF_BYCOMMAND | MF_STRING, id as usize,
                                        PCWSTR(wide(&key_label(c, a)).as_ptr()));
                }
            } else if opening == m.pad {
                CheckMenuItem(m.pad, ID_PAD_ENABLE, (MF_BYCOMMAND | if c.pad_enabled { MF_CHECKED } else { MF_UNCHECKED }).0);
                let _ = ModifyMenuW(m.pad, ID_PAD_STATUS, MF_BYCOMMAND | MF_STRING | MF_GRAYED, ID_PAD_STATUS as usize,
                                    PCWSTR(wide(&c.pad_name).as_ptr()));
                for (i, sub) in m.pad_inputs.iter().enumerate() {
                    let _ = ModifyMenuW(m.pad, 3 + i as u32, MF_BYPOSITION | MF_POPUP, sub.0 as usize,
                                        PCWSTR(wide(&pad_label(c, i)).as_ptr()));
                }
            } else if let Some(i) = m.pad_inputs.iter().position(|s| *s == opening) {
                let first = ID_PAD_MAP + i as u32 * 32;
                let _ = CheckMenuRadioItem(opening, first, first + ACTIONS.len() as u32 - 1,
                                           first + c.pad_map[i] as u32, MF_BYCOMMAND.0);
            }
        });
    });
}

/// Handles one of our menu commands; false if `id` isn't ours.
pub unsafe fn on_command(hwnd: HWND, id: u32) -> bool {
    if id < ID_KEY_RESET {
        return false;
    }
    let _ = with(|c| match id {
        ID_KEY_RESET => c.reset_keys(),
        ID_PAD_RESET => c.reset_pad(),
        ID_PAD_ENABLE => {
            c.pad_enabled = !c.pad_enabled;
            c.save();
        }
        id if (ID_KEY_BIND..ID_PAD_MAP).contains(&id) => {
            let action = (id - ID_KEY_BIND) as usize;
            CAPTURING.with(|cap| cap.set(Some(action)));
            let prompt = format!("Press a key for {} (Esc cancels)", ACTIONS[action].0);
            let _ = SetWindowTextW(hwnd, PCWSTR(wide(&prompt).as_ptr()));
        }
        id if id >= ID_PAD_MAP => {
            let (i, a) = (((id - ID_PAD_MAP) / 32) as usize, ((id - ID_PAD_MAP) % 32) as usize);
            if i < INPUTS.len() && a < ACTIONS.len() {
                c.pad_map[i] = a;
                c.save();
            }
        }
        _ => {}
    });
    true
}

/// The prompt to keep in the title bar while waiting for a key.
pub fn capture_prompt() -> Option<String> {
    CAPTURING.with(|c| c.get()).map(|a| format!("Press a key for {} (Esc cancels)", ACTIONS[a].0))
}

/// What the window procedure should do with a key message.
pub enum Key {
    /// Used up here (key binding in progress, or a game-control key nobody is bound to).
    Consumed,
    /// Send this message on to the game.
    Forward(u32, usize, isize),
}

pub fn on_key(msg: u32, vk: u16, lparam: isize) -> Key {
    if let Some(action) = CAPTURING.with(|c| c.get()) {
        if msg == WM_KEYDOWN {
            CAPTURING.with(|c| c.set(None));
            if vk != VK_ESCAPE.0 {
                let _ = with(|c| c.bind_key(action, vk));
            }
        }
        LAST_KEY_PASSED.with(|p| p.set(false));
        return Key::Consumed;
    }
    if msg == WM_CHAR {
        // Characters only follow keys that went through unchanged.
        return if LAST_KEY_PASSED.with(|p| p.get()) { Key::Forward(msg, vk as usize, lparam) } else { Key::Consumed };
    }
    let route = with(|c| c.route_key(vk)).unwrap_or(KeyRoute::Pass);
    if msg == WM_KEYDOWN {
        LAST_KEY_PASSED.with(|p| p.set(matches!(route, KeyRoute::Pass)));
    }
    match route {
        KeyRoute::Pass => Key::Forward(msg, vk as usize, lparam),
        KeyRoute::Swallow => Key::Consumed,
        KeyRoute::Send(game_vk) => {
            let down = msg == WM_KEYDOWN;
            // Keep the auto-repeat / previous-state bits of the real key press.
            let l = controls::key_lparam(game_vk, down) | (lparam & 0x4000_0000) | (lparam & 0xFFFF);
            Key::Forward(msg, game_vk as usize, l)
        }
    }
}

/// Polls the controller; returns (message, vk, lparam) key events for the game.
pub fn poll_pad(focused: bool) -> Vec<(u32, usize, isize)> {
    with(|c| c.poll(focused)).unwrap_or_default().into_iter()
        .map(|(vk, down)| (if down { WM_KEYDOWN } else { WM_KEYUP }, vk as usize, controls::key_lparam(vk, down)))
        .collect()
}
