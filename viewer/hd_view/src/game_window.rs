//! The game's own window, seen from the viewer: found by process, hidden while the viewer shows
//! the game, with its menu bar mirrored into the viewer and keyboard input passed through.
//!
//! The game keeps running (and drawing its 320x240 frame) with its window hidden; the viewer is
//! the only window the player sees. Closing the viewer closes the game, and the viewer quits when
//! the game does.

use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::{CreateProcessW, OpenProcess, QueryFullProcessImageNameW, PROCESS_CREATION_FLAGS,
                                        PROCESS_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                                        STARTF_USESHOWWINDOW, STARTUPINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;

const GAME_EXE: &str = "stdrt.exe";

/// The hidden game window, for the panic handler to show again (0 = none).
pub static HIDDEN_GAME: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// If the viewer dies, the game must not be left running with its window hidden.
pub fn show_hidden_game() {
    let h = HIDDEN_GAME.load(std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = ShowWindow(HWND(h as _), SW_SHOW);
        }
    }
}

pub struct GameWindow {
    pub hwnd: HWND,
    /// The game's menu bar (owned by the game process; read only).
    game_menu: HMENU,
    /// Our copy of it, attached to the viewer window.
    pub menu: Option<HMENU>,
}

/// Starts the game runtime (`stdrt.exe` in `runtime_dir`, reading the game data from `game_exe`
/// at `pack_offset`) with its window hidden from the start. Returns the process id.
pub fn launch(runtime_dir: &Path, game_exe: &Path, pack_offset: u32) -> Result<u32, String> {
    let exe = runtime_dir.join(GAME_EXE);
    let mut cmd: Vec<u16> = format!("\"{}\" /SF \"{}\" /SO{pack_offset}", exe.display(), game_exe.display())
        .encode_utf16().chain(Some(0)).collect();
    let dir: Vec<u16> = runtime_dir.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESHOWWINDOW,
        wShowWindow: SW_HIDE.0 as u16,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(PCWSTR::null(), PWSTR(cmd.as_mut_ptr()), None, None, false, PROCESS_CREATION_FLAGS(0), None,
                       PCWSTR(dir.as_ptr()), &si, &mut pi)
            .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(pi.dwProcessId)
}

/// Finds the game's main window. With `pid` (a game we launched), its first top-level window with
/// a caption, visible or not; otherwise a visible top-level window of any stdrt.exe process.
pub fn find(pid: Option<u32>) -> Option<HWND> {
    struct Search(Option<HWND>, Option<u32>);
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if GetWindow(hwnd, GW_OWNER).is_ok() {
            return TRUE;
        }
        let found = match search.1 {
            Some(pid) => {
                let mut owner = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut owner));
                owner == pid && GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0
            }
            None => IsWindowVisible(hwnd).as_bool() && is_game_process(hwnd),
        };
        if found {
            search.0 = Some(hwnd);
            return FALSE;
        }
        TRUE
    }
    let mut search = Search(None, pid);
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.0
}

unsafe fn is_game_process(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(process);
    ok && String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase().ends_with(&format!("\\{GAME_EXE}"))
}

impl GameWindow {
    /// Takes over `hwnd`: hides it and builds a copy of its menu bar if it has one yet (a game
    /// started hidden may attach its menu a moment later; see `copy_menu_if_ready`).
    pub fn attach(hwnd: HWND) -> GameWindow {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        HIDDEN_GAME.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        let mut game = GameWindow { hwnd, game_menu: HMENU::default(), menu: None };
        game.copy_menu_if_ready();
        game
    }

    /// Copies the game's menu bar into `self.menu` once the game has one. True if it did now.
    pub fn copy_menu_if_ready(&mut self) -> bool {
        if !self.game_menu.is_invalid() {
            return false;
        }
        unsafe {
            let game_menu = GetMenu(self.hwnd);
            if game_menu.is_invalid() || GetMenuItemCount(game_menu) <= 0 {
                return false;
            }
            self.game_menu = game_menu;
            self.menu = copy_menu(game_menu, true);
        }
        self.menu.is_some()
    }

    pub fn alive(&self) -> bool {
        unsafe { IsWindow(self.hwnd).as_bool() }
    }

    /// Hides the game window again if the game showed it (it does on some mode changes).
    pub fn keep_hidden(&self) {
        unsafe {
            if IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Sends a menu command chosen in the viewer's copy of the menu to the game.
    pub fn command(&self, id: u32) {
        unsafe {
            let _ = PostMessageW(self.hwnd, WM_COMMAND, WPARAM(id as usize), LPARAM(0));
        }
    }

    /// Before one of our popup menus opens, copy check marks and enabled states from the game's.
    pub fn sync_popup(&self, popup: HMENU) {
        unsafe {
            for i in 0..GetMenuItemCount(popup).max(0) {
                let id = GetMenuItemID(popup, i);
                if id == u32::MAX {
                    continue; // submenu: synced when it opens
                }
                let state = GetMenuState(self.game_menu, id, MF_BYCOMMAND);
                if state == u32::MAX {
                    continue;
                }
                let checked = if state & MF_CHECKED.0 != 0 { MF_CHECKED } else { MF_UNCHECKED };
                CheckMenuItem(popup, id, (MF_BYCOMMAND | checked).0);
                let enabled = if state & (MF_GRAYED.0 | MF_DISABLED.0) != 0 { MF_GRAYED } else { MF_ENABLED };
                let _ = EnableMenuItem(popup, id, MF_BYCOMMAND | enabled);
            }
        }
    }

    /// Passes a keyboard message from the viewer window to the game.
    pub fn forward_key(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) {
        unsafe {
            let _ = PostMessageW(self.input_target(), msg, wparam, lparam);
        }
    }

    /// The window in the game that takes keyboard input: its focus window if it has one,
    /// otherwise the main window.
    fn input_target(&self) -> HWND {
        unsafe {
            let thread = GetWindowThreadProcessId(self.hwnd, None);
            let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
            if GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndFocus.is_invalid() {
                return info.hwndFocus;
            }
            self.hwnd
        }
    }

    /// Shows the game window again (used if the viewer quits while the game keeps running).
    pub fn release(&self) {
        HIDDEN_GAME.store(0, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
    }

    pub fn close(&self) {
        unsafe {
            let _ = PostMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Recursively copies a menu (text, command ids, separators, submenus, states).
unsafe fn copy_menu(src: HMENU, bar: bool) -> Option<HMENU> {
    let dst = if bar { CreateMenu().ok()? } else { CreatePopupMenu().ok()? };
    for i in 0..GetMenuItemCount(src).max(0) {
        let mut text = [0u16; 256];
        let mut info = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_STRING,
            dwTypeData: PWSTR(text.as_mut_ptr()),
            cch: text.len() as u32 - 1,
            ..Default::default()
        };
        if GetMenuItemInfoW(src, i as u32, true, &mut info).is_err() {
            continue;
        }
        if !info.hSubMenu.is_invalid() {
            info.hSubMenu = copy_menu(info.hSubMenu, false).unwrap_or_default();
        }
        info.dwTypeData = PWSTR(text.as_mut_ptr());
        let _ = InsertMenuItemW(dst, i as u32, true, &info);
    }
    Some(dst)
}
