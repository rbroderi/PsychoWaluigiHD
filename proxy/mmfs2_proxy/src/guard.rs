//! Works around a crash in stdrt.exe 3.0.251.1 itself (it happens with the original mmfs2.dll too).
//!
//! The game binds plain Enter to the runtime's Pause command (WM_COMMAND 0x3f4). Its handler
//! shows the "Application paused" dialog and, once Continue is pressed, fires the storyboard
//! event (-3,-8) through the current frame's event tables. While a frame transition runs, the
//! runtime pumps window messages every step (stdrt+0x18420) and the previous frame's event
//! tables are already freed, so a pause there walks freed memory and faults in the event walker
//! (stdrt+0x42700, `mov ecx,[edi]` / `test byte ptr [ecx+5],0x40`). The guard subclasses the
//! main window and holds Pause back while a transition is active; the SpriteDraw hook re-posts it
//! once the frame is running again.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicUsize, Ordering};

const GWL_WNDPROC: i32 = -4;
const WM_COMMAND: u32 = 0x111;
const ID_PAUSE: usize = 0x3f4;

/// stdrt.exe globals (image base 0x400000, not relocated).
const G_MAIN_HWND: usize = 0x452588;
/// Frame-loop state; +0x70 holds the active transition object, or 0.
const G_FRAME_STATE: usize = 0x4525b0;
const TRANSITION_OFF: usize = 0x70;
/// The Pause handler's "fire the resume event" sequence, checked before installing.
const SIG_ADDR: usize = 0x43ddd7;
const SIG: [u8; 31] = [
    0xa1, 0xb4, 0x25, 0x45, 0x00, 0x85, 0xc0, 0x74, 0x19, 0x8b, 0x88, 0xd0, 0x00, 0x00, 0x00, 0x68,
    0xfd, 0xff, 0xf8, 0xff, 0x89, 0x88, 0x10, 0x04, 0x00, 0x00, 0xe8, 0xaa, 0x44, 0x00, 0x00,
];

#[link(name = "user32")]
extern "system" {
    fn SetWindowLongA(hwnd: isize, index: i32, value: isize) -> isize;
    fn CallWindowProcA(prev: isize, hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize;
    fn IsWindow(hwnd: isize) -> i32;
    fn PostMessageA(hwnd: isize, msg: u32, wp: usize, lp: isize) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

static TRIED: AtomicBool = AtomicBool::new(false);
static PREV_PROC: AtomicIsize = AtomicIsize::new(0);
/// Pause commands held back so far (logged by the SpriteDraw hook).
pub static DEFERRED: AtomicU32 = AtomicU32::new(0);
/// A held-back Pause: its wParam, or 0.
static PENDING_WP: AtomicUsize = AtomicUsize::new(0);

/// Called from the game thread (SpriteDraw); installs the guard once the main window exists.
/// Returns a line for the event log the first time it decides something.
pub fn install() -> Option<String> {
    if TRIED.load(Ordering::Relaxed) {
        return None;
    }
    if std::env::var_os("MMFS2_NO_PAUSE_GUARD").is_some() {
        TRIED.store(true, Ordering::Relaxed);
        return Some("pause guard: disabled by MMFS2_NO_PAUSE_GUARD".into());
    }
    unsafe {
        if GetModuleHandleW(std::ptr::null()) != 0x400000
            || std::slice::from_raw_parts(SIG_ADDR as *const u8, SIG.len()) != SIG
        {
            TRIED.store(true, Ordering::Relaxed);
            return Some("pause guard: unknown stdrt.exe build, not installed".into());
        }
        let hwnd = *(G_MAIN_HWND as *const isize);
        if hwnd == 0 || IsWindow(hwnd) == 0 {
            return None; // try again next frame
        }
        TRIED.store(true, Ordering::Relaxed);
        let prev = SetWindowLongA(hwnd, GWL_WNDPROC, guard_proc as usize as isize);
        if prev == 0 {
            return Some("pause guard: SetWindowLong failed".into());
        }
        PREV_PROC.store(prev, Ordering::Relaxed);
        Some(format!("pause guard: installed on hwnd {hwnd:#x}, previous proc {prev:#x}"))
    }
}

unsafe fn in_transition() -> bool {
    let state = *(G_FRAME_STATE as *const usize);
    state != 0 && *((state + TRANSITION_OFF) as *const usize) != 0
}

unsafe extern "system" fn guard_proc(hwnd: isize, msg: u32, wp: usize, lp: isize) -> isize {
    if msg == WM_COMMAND && wp & 0xFFFF == ID_PAUSE && in_transition() {
        DEFERRED.fetch_add(1, Ordering::Relaxed);
        PENDING_WP.store(wp, Ordering::Relaxed);
        return 0;
    }
    CallWindowProcA(PREV_PROC.load(Ordering::Relaxed), hwnd, msg, wp, lp)
}

/// Called from the SpriteDraw hook: re-posts a held-back Pause once no transition is running.
pub fn flush_pending() {
    unsafe {
        if PENDING_WP.load(Ordering::Relaxed) == 0 || in_transition() {
            return;
        }
        let wp = PENDING_WP.swap(0, Ordering::Relaxed);
        PostMessageA(*(G_MAIN_HWND as *const isize), WM_COMMAND, wp, 0);
    }
}
