//! Proxy for MMFS2.dll. Most exports are thunks that count the call and jump to the same
//! ordinal in mmfs2_orig.dll; the sprite and surface API goes through typed hooks (hooks.rs)
//! that feed the HD viewer.
//!
//! Debug output is only written when the MMFS2_CAPTURE environment variable is set: a background
//! thread then logs per-second call counts to mmfs2_calls.log (which exports the runtime uses),
//! and hooks.rs writes its capture logs.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

mod guard;
mod hooks;
mod masks;
mod publish;
mod names;
mod thunks;

use thunks::{CODE_ORDINALS, MAX_ORDINAL};

#[no_mangle]
pub static COUNTS: [AtomicU32; MAX_ORDINAL + 1] = [const { AtomicU32::new(0) }; MAX_ORDINAL + 1];
#[no_mangle]
pub static mut REAL: [usize; MAX_ORDINAL + 1] = [0; MAX_ORDINAL + 1];
/// Addresses of the original's data exports (vtables), e.g. to tell CFillData subclasses apart.
pub static mut REAL_DATA: [usize; MAX_ORDINAL + 1] = [0; MAX_ORDINAL + 1];

type Hmodule = isize;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> Hmodule;
    fn GetProcAddress(module: Hmodule, name: *const u8) -> usize;
    fn GetModuleFileNameW(module: Hmodule, buf: *mut u16, size: u32) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, flags: u32) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn module_dir(module: Hmodule) -> std::path::PathBuf {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

fn fail(msg: &str) -> ! {
    unsafe {
        MessageBoxW(0, wide(msg).as_ptr(), wide("mmfs2 proxy").as_ptr(), 0x10);
    }
    std::process::abort();
}

unsafe fn resolve(self_module: Hmodule) {
    let orig = module_dir(self_module).join("mmfs2_orig.dll");
    let orig_w: Vec<u16> = orig.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let real = LoadLibraryW(orig_w.as_ptr());
    if real == 0 {
        fail(&format!("could not load {}", orig.display()));
    }
    let table = &mut *std::ptr::addr_of_mut!(REAL);
    for &ordinal in CODE_ORDINALS.iter() {
        let addr = GetProcAddress(real, ordinal as usize as *const u8);
        if addr == 0 {
            fail(&format!("mmfs2_orig.dll has no ordinal {ordinal}"));
        }
        table[ordinal as usize] = addr;
    }
    for (ordinal, copy) in thunks::data_exports() {
        let addr = GetProcAddress(real, ordinal as usize as *const u8);
        if addr == 0 {
            fail(&format!("mmfs2_orig.dll has no ordinal {ordinal}"));
        }
        std::ptr::copy_nonoverlapping(addr as *const usize, (*copy).as_mut_ptr(), thunks::DATA_WORDS);
        (*std::ptr::addr_of_mut!(REAL_DATA))[ordinal as usize] = addr;
    }
}

fn logger(dir: std::path::PathBuf) {
    let Ok(mut log) = File::create(dir.join("mmfs2_calls.log")) else { return };
    let start = Instant::now();
    let mut last = vec![0u32; MAX_ORDINAL + 1];
    let mut totals = vec![0u64; MAX_ORDINAL + 1];
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let mut rows: Vec<(u32, usize)> = Vec::new();
        for &o in CODE_ORDINALS.iter() {
            let o = o as usize;
            let now = COUNTS[o].load(Ordering::Relaxed);
            let delta = now.wrapping_sub(last[o]);
            last[o] = now;
            if delta > 0 {
                totals[o] += delta as u64;
                rows.push((delta, o));
            }
        }
        rows.sort_by(|a, b| b.cmp(a));
        let _ = writeln!(log, "== t={}s", start.elapsed().as_secs());
        for (n, o) in &rows {
            let _ = writeln!(log, "{n:>8}  #{o:<4} {}", names::name(*o));
        }
        // Rewrite the cumulative summary every second so it survives the game being killed.
        if let Ok(mut summary) = File::create(dir.join("mmfs2_calls_total.log")) {
            let mut all: Vec<(u64, usize)> =
                totals.iter().enumerate().filter(|(_, &t)| t > 0).map(|(o, &t)| (t, o)).collect();
            all.sort_by(|a, b| b.cmp(a));
            for (n, o) in all {
                let _ = writeln!(summary, "{n:>10}  #{o:<4} {}", names::name(o));
            }
        }
        let _ = log.flush();
    }
}

#[no_mangle]
pub extern "system" fn DllMain(module: Hmodule, reason: u32, _reserved: usize) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        unsafe { resolve(module) };
        let dir = module_dir(module);
        hooks::init(dir.clone());
        if hooks::logging() {
            std::thread::spawn(move || logger(dir));
        }
    }
    1
}
