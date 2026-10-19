// A windowed app: no console window of its own.
#![windows_subsystem = "windows"]
//! HD viewer for Psycho Waluigi.
//!
//! Reads the frames the mmfs2 proxy publishes (sprite list, background ops, low-res screen)
//! and redraws them at `--scale` times the game's resolution with HD textures.
//!
//! Start the viewer and it starts the game (stdrt.exe next to it, with its window hidden), then
//! takes its place: the game's menu bar is copied into the viewer and keyboard input is passed
//! through, so the viewer is the only window. Closing the viewer closes the game. If a game is
//! already running, the viewer takes over that one instead.
//!
//! Viewer keys (everything else goes to the game, including its own F1 help, F2 reset and Enter pause):
//!   F6 = cycle the look: xBRZ art, cleanEdge art, original pixels; F9 = save screenshots,
//!   F7 = mark background pixels the HD rebuild doesn't cover (cyan) or doesn't reproduce (magenta),
//!   Alt+Enter / View > Full Screen = borderless fullscreen (Esc leaves it),
//!   F8 / View > Hide the menu = show or hide the menu bar.
//! Controls: the viewer's Controls menu replaces the game's Configure Controls dialog. Keyboard
//! keys can be rebound per game action, and an XInput pad (e.g. an 8BitDo in X-input mode)
//! drives the game while the viewer has focus, with every button and direction mappable. Saved in
//! controls.cfg next to the viewer. The game itself stays on its default keyboard controls.
//! Those two View items are the game's, but they act on the viewer: done by the hidden game
//! window they would only switch its display mode.
//!
//! Options (paths default to folders next to hd_runtime/):
//!   --scale N            scene scale (default 9: 320x240 -> 2880x2160)
//!   --sprites DIR        original extracted sprites (default ../sprites)
//!   --hd-xbrz DIR        xBRZ HD art (default ../sprites_xbrz9x_direct)
//!   --hd-cleanedge DIR   cleanEdge HD art (default ../sprites_cleanedge9x)
//!   --budget-mb N        HD texture memory budget (default 3072)
//!   --shots DIR          where P / --auto-shot save (default ./shots)
//!   --auto-shot SECS     also save screenshots every SECS seconds
//!   --exit-after SECS    quit after SECS seconds (for scripted runs)
//!   --stats FILE         log per-second performance numbers to FILE
//!   --debug-residual     start with F7 on
//!   --no-takeover        leave the game's window alone (side by side, as before)
//!   --no-forward         don't pass keys to the game (it then needs its own window focused)
//!   --no-launch          don't start the game; wait for one started separately
//!   --game PATH          the game exe holding the data (default ../psychowaluigi.exe)
//!   --pack-offset N      where the runtime pack starts in that exe (default 94208)

mod controls;
mod controls_menu;
mod game_window;
mod gfx;
mod shm;
mod textures;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_F6, VK_F7, VK_F8, VK_F9, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;

use controls_menu::Key;
use game_window::GameWindow;
use gfx::{Gfx, View};
use shm::{Reader, Snapshot};
use textures::Cache;

static RESIZE: AtomicU32 = AtomicU32::new(0);
/// Viewer hotkeys pressed since the last frame.
static KEYS: Mutex<Vec<Hotkey>> = Mutex::new(Vec::new());
static FORWARD_KEYS: AtomicBool = AtomicBool::new(true);
/// Set while the viewer is fullscreen, so Esc leaves fullscreen instead of going to the game.
static FULLSCREEN: AtomicBool = AtomicBool::new(false);
/// Menu bar hidden with F8 / View > Hide the menu.
static MENU_HIDDEN: AtomicBool = AtomicBool::new(false);

/// The game's View menu commands (MMF2 runtime ids), handled by the viewer instead.
const CMD_HIDE_MENU: u32 = 1022;
const CMD_FULL_SCREEN: u32 = 1025;

thread_local! {
    /// The game window we stand in for, once found (UI thread only).
    static GAME: RefCell<Option<GameWindow>> = const { RefCell::new(None) };
    /// Menu bar used before the game (and its menu) appears.
    static OWN_BAR: RefCell<Option<HMENU>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
enum Hotkey {
    ToggleView,
    ToggleResidual,
    Screenshot,
    Fullscreen,
    ToggleMenu,
}

fn hotkey(msg: u32, vk: u32, lparam: LPARAM) -> Option<Hotkey> {
    let alt = lparam.0 & (1 << 29) != 0;
    match (msg, vk) {
        (WM_KEYDOWN, k) if k == VK_F6.0 as u32 => Some(Hotkey::ToggleView),
        (WM_KEYDOWN, k) if k == VK_F7.0 as u32 => Some(Hotkey::ToggleResidual),
        (WM_KEYDOWN, k) if k == VK_F8.0 as u32 => Some(Hotkey::ToggleMenu),
        (WM_KEYDOWN, k) if k == VK_F9.0 as u32 => Some(Hotkey::Screenshot),
        (WM_SYSKEYDOWN, k) if k == VK_RETURN.0 as u32 && alt => Some(Hotkey::Fullscreen),
        (WM_KEYDOWN, k) if k == VK_ESCAPE.0 as u32 && FULLSCREEN.load(Ordering::Relaxed) => Some(Hotkey::Fullscreen),
        _ => None,
    }
}

/// Keys that belong to the viewer (or its own key-up) are kept; the rest go to the game.
fn is_viewer_key(vk: u32) -> bool {
    [VK_F6.0, VK_F7.0, VK_F8.0, VK_F9.0].contains(&(vk as u16))
        || (vk == VK_ESCAPE.0 as u32 && FULLSCREEN.load(Ordering::Relaxed))
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                RESIZE.store(lparam.0 as u32, Ordering::Relaxed);
                LRESULT(0)
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_CHAR => {
                let vk = wparam.0 as u32;
                if let Some(k) = hotkey(msg, vk, lparam) {
                    KEYS.lock().unwrap().push(k);
                    return LRESULT(0);
                }
                let game_key = matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_CHAR) && !is_viewer_key(vk);
                if game_key && FORWARD_KEYS.load(Ordering::Relaxed) {
                    // Through the player's key bindings (or into a binding being set).
                    if let Key::Forward(m, w, l) = controls_menu::on_key(msg, vk as u16, lparam.0) {
                        GAME.with(|g| {
                            if let Some(game) = g.borrow().as_ref() {
                                game.forward_key(m, WPARAM(w), LPARAM(l));
                            }
                        });
                    }
                    return LRESULT(0);
                }
                // Alt / F10 still open our menu bar.
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_INITMENUPOPUP => {
                controls_menu::on_init_popup(HMENU(wparam.0 as _));
                GAME.with(|g| {
                    if let Some(game) = g.borrow().as_ref() {
                        game.sync_popup(HMENU(wparam.0 as _));
                    }
                });
                LRESULT(0)
            }
            WM_COMMAND if lparam.0 == 0 && (wparam.0 >> 16) & 0xFFFF == 0 => {
                // A choice from the copied menu: the game does the work, except for the View
                // commands, which are about the window the player is looking at.
                let id = (wparam.0 & 0xFFFF) as u32;
                if controls_menu::on_command(hwnd, id) || id == controls_menu::CMD_CONFIGURE_CONTROLS {
                    return LRESULT(0);
                }
                match id {
                    CMD_FULL_SCREEN => KEYS.lock().unwrap().push(Hotkey::Fullscreen),
                    CMD_HIDE_MENU => KEYS.lock().unwrap().push(Hotkey::ToggleMenu),
                    id => GAME.with(|g| {
                        if let Some(game) = g.borrow().as_ref() {
                            game.command(id);
                        }
                    }),
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                GAME.with(|g| {
                    if let Some(game) = g.borrow().as_ref() {
                        game.close();
                    }
                });
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

struct Args {
    scale: u32,
    sprites: PathBuf,
    hd_xbrz: PathBuf,
    hd_cleanedge: PathBuf,
    budget: usize,
    shots: PathBuf,
    auto_shot: Option<f32>,
    exit_after: Option<f32>,
    stats: Option<PathBuf>,
    debug_residual: bool,
    takeover: bool,
    forward: bool,
    launch: bool,
    game: PathBuf,
    pack_offset: u32,
}

fn parse_args() -> Args {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
    let root = exe_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| exe_dir.clone());
    let mut a = Args {
        scale: 9,
        sprites: root.join("sprites"),
        hd_xbrz: root.join("sprites_xbrz9x_direct"),
        hd_cleanedge: root.join("sprites_cleanedge9x"),
        budget: 3072 << 20,
        shots: exe_dir.join("shots"),
        auto_shot: None,
        exit_after: None,
        stats: None,
        debug_residual: false,
        takeover: true,
        forward: true,
        launch: true,
        game: root.join("psychowaluigi.exe"),
        pack_offset: 94208,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--debug-residual" => a.debug_residual = true,
            "--no-takeover" => a.takeover = false,
            "--no-forward" => a.forward = false,
            "--no-launch" => a.launch = false,
            _ => {}
        }
        if matches!(k.as_str(), "--debug-residual" | "--no-takeover" | "--no-forward" | "--no-launch") {
            continue;
        }
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--scale" => a.scale = v.parse().unwrap_or(9).clamp(1, 12),
            "--sprites" => a.sprites = v.into(),
            "--hd-xbrz" => a.hd_xbrz = v.into(),
            "--hd-cleanedge" => a.hd_cleanedge = v.into(),
            "--budget-mb" => a.budget = v.parse::<usize>().unwrap_or(3072) << 20,
            "--shots" => a.shots = v.into(),
            "--auto-shot" => a.auto_shot = v.parse().ok(),
            "--exit-after" => a.exit_after = v.parse().ok(),
            "--stats" => a.stats = Some(v.into()),
            "--game" => a.game = v.into(),
            "--pack-offset" => a.pack_offset = v.parse().unwrap_or(94208),
            _ => eprintln!("unknown option {k}"),
        }
    }
    a
}

/// Without a console, errors have to be shown.
fn fatal(msg: &str) -> ! {
    let text: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Psycho Waluigi HD"), MB_ICONERROR);
    }
    std::process::exit(1);
}

struct Fullscreen {
    saved: Option<(i32, RECT)>,
}

impl Fullscreen {
    unsafe fn toggle(&mut self, hwnd: HWND) {
        if let Some((style, rc)) = self.saved.take() {
            SetWindowLongW(hwnd, GWL_STYLE, style);
            let _ = SetWindowPos(hwnd, HWND_NOTOPMOST, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top,
                                 SWP_FRAMECHANGED);
            FULLSCREEN.store(false, Ordering::Relaxed);
            update_menu(hwnd);
            return;
        }
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        self.saved = Some((GetWindowLongW(hwnd, GWL_STYLE), rc));
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
        let m = mi.rcMonitor;
        SetWindowLongW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as i32);
        FULLSCREEN.store(true, Ordering::Relaxed);
        update_menu(hwnd);
        let _ = SetWindowPos(hwnd, HWND_TOP, m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_FRAMECHANGED);
    }
}

/// Finishes the copied game menu: the viewer's Controls menu replaces the game's controls dialog.
unsafe fn install_game_menu(game: &GameWindow) {
    if let Some(bar) = game.menu {
        let _ = DeleteMenu(bar, controls_menu::CMD_CONFIGURE_CONTROLS, MF_BYCOMMAND);
        controls_menu::add_to(bar);
    }
}

/// The menu bar (the game's copied menu plus Controller, or Controller alone before the game
/// appears) sits on the viewer window unless fullscreen or hidden with F8.
unsafe fn update_menu(hwnd: HWND) {
    let visible = !FULLSCREEN.load(Ordering::Relaxed) && !MENU_HIDDEN.load(Ordering::Relaxed);
    let bar = GAME.with(|g| g.borrow().as_ref().and_then(|game| game.menu))
        .or_else(|| OWN_BAR.with(|b| *b.borrow()));
    if let Some(bar) = bar {
        let _ = SetMenu(hwnd, if visible { bar } else { HMENU::default() });
        let _ = DrawMenuBar(hwnd);
    }
}

/// Per-second accumulators for --stats.
#[derive(Default)]
struct Perf {
    loops: u32,
    build_ms: f32,
    present_ms: f32,
    worst_ms: f32,
    draw_calls: u32,
}

fn save_shots(gfx: &Gfx, snap: &Snapshot, dir: &PathBuf) {
    let _ = std::fs::create_dir_all(dir);
    let hd = dir.join(format!("f{:07}_hd.png", snap.frame));
    if let Err(e) = gfx.save_scene(&hd) {
        eprintln!("screenshot failed: {e}");
        return;
    }
    if snap.width > 0 {
        let rgb: Vec<u8> = snap.screen.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
        let _ = gfx::write_png(&dir.join(format!("f{:07}_orig.png", snap.frame)), snap.width, snap.height,
                               png::ColorType::Rgb, &rgb);
    }
    println!("saved {}", hd.display());
}

fn main() -> windows::core::Result<()> {
    // A windowed app has nowhere to print a panic: give the game its window back and say what happened.
    std::panic::set_hook(Box::new(|info| {
        game_window::show_hidden_game();
        let text: Vec<u16> = format!("The HD viewer stopped: {info}").encode_utf16().chain(Some(0)).collect();
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Psycho Waluigi HD"), MB_ICONERROR);
        }
    }));
    let args = parse_args();
    let styles = vec![("xBRZ".to_string(), args.hd_xbrz.clone()), ("cleanEdge".to_string(), args.hd_cleanedge.clone())];
    let mut cache = match Cache::new(args.sprites.clone(), styles, args.budget) {
        Ok(c) => c,
        Err(e) => {
            fatal(&format!("Cannot load the sprite index: {e}"));
        }
    };

    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("PsychoWaluigiHD");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Default client area: 4:3 at three quarters of the primary screen height.
        let client_h = (GetSystemMetrics(SM_CYSCREEN) * 3 / 4).max(480);
        let client_w = client_h * 4 / 3;
        let mut rc = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        AdjustWindowRect(&mut rc, WS_OVERLAPPEDWINDOW, false)?;
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Psycho Waluigi HD"), WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                                   CW_USEDEFAULT, CW_USEDEFAULT, rc.right - rc.left, rc.bottom - rc.top, None, None,
                                   instance, None)?;

        let mut gfx = Gfx::new(hwnd, client_w as u32, client_h as u32, args.scale)?;
        gfx.debug_residual = args.debug_residual;
        FORWARD_KEYS.store(args.forward, Ordering::Relaxed);
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
        controls_menu::init(exe_dir.join("controls.cfg"));
        if let Ok(bar) = CreateMenu() {
            controls_menu::add_to(bar);
            OWN_BAR.with(|b| *b.borrow_mut() = Some(bar));
        }
        update_menu(hwnd);
        let mut last_game_search = Instant::now() - Duration::from_secs(1);
        let mut took_over = false;
        // Start the game ourselves (hidden) unless one is already running or we were told not to.
        let mut launched_pid = None;
        if args.takeover && args.launch && game_window::find(None).is_none() {
            match game_window::launch(&exe_dir, &args.game, args.pack_offset) {
                Ok(pid) => launched_pid = Some(pid),
                Err(e) => fatal(&e),
            }
        }
        let mut reader: Option<Reader> = None;
        let mut last_open_try = Instant::now() - Duration::from_secs(1);
        let mut snap = Snapshot::default();
        let mut view = View::Hd;
        let mut fullscreen = Fullscreen { saved: None };
        let start = Instant::now();
        let mut last_auto = Instant::now();
        let mut fps_frames = 0u32;
        let mut fps_clock = Instant::now();
        let mut fps = 0.0;
        let mut perf = Perf::default();
        let mut stats_log = args.stats.as_ref().and_then(|p| std::fs::File::create(p).ok());

        loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Some(limit) = args.exit_after {
                if start.elapsed().as_secs_f32() > limit {
                    // Scripted runs: give the game its window back rather than closing it.
                    GAME.with(|g| if let Some(game) = g.borrow().as_ref() { game.release() });
                    return Ok(());
                }
            }

            // Stand in for the game window once it appears; quit when the game does.
            if args.takeover {
                if took_over {
                    let state = GAME.with(|g| g.borrow_mut().as_mut().map(|game| {
                        game.keep_hidden();
                        // A game started hidden attaches its menu a little after its window exists.
                        let new_menu = game.copy_menu_if_ready();
                        if new_menu {
                            install_game_menu(game);
                        }
                        (game.alive(), new_menu)
                    }));
                    match state {
                        Some((true, new_menu)) => {
                            if new_menu {
                                update_menu(hwnd);
                            }
                        }
                        _ => return Ok(()),
                    }
                } else if last_game_search.elapsed() > Duration::from_millis(20) {
                    last_game_search = Instant::now();
                    if let Some(game_hwnd) = game_window::find(launched_pid) {
                        let game = GameWindow::attach(game_hwnd);
                        if game.menu.is_some() {
                            install_game_menu(&game);
                        }
                        GAME.with(|g| *g.borrow_mut() = Some(game));
                        update_menu(hwnd);
                        let _ = SetForegroundWindow(hwnd);
                        took_over = true;
                    }
                }
            }

            let size = RESIZE.swap(0, Ordering::Relaxed);
            if size != 0 {
                gfx.resize(size & 0xFFFF, size >> 16)?;
            }
            // Controller -> game keys, only while the viewer is the window being played.
            let focused = GetForegroundWindow() == hwnd;
            let pad_keys = controls_menu::poll_pad(focused);
            if !pad_keys.is_empty() {
                GAME.with(|g| {
                    if let Some(game) = g.borrow().as_ref() {
                        for (msg, vk, lparam) in pad_keys {
                            game.forward_key(msg, WPARAM(vk), LPARAM(lparam));
                        }
                    }
                });
            }

            let keys: Vec<Hotkey> = std::mem::take(&mut *KEYS.lock().unwrap());
            for k in keys {
                match k {
                    // xBRZ -> cleanEdge -> original pixels -> xBRZ (styles whose art is missing are skipped).
                    Hotkey::ToggleView => {
                        if view == View::Original {
                            view = View::Hd;
                            cache.style = 0;
                        } else if cache.style + 1 < cache.styles().len() {
                            cache.style += 1;
                        } else {
                            view = View::Original;
                        }
                    }
                    Hotkey::Fullscreen => fullscreen.toggle(hwnd),
                    Hotkey::Screenshot => save_shots(&gfx, &snap, &args.shots),
                    Hotkey::ToggleResidual => gfx.debug_residual = !gfx.debug_residual,
                    Hotkey::ToggleMenu => {
                        MENU_HIDDEN.store(!MENU_HIDDEN.load(Ordering::Relaxed), Ordering::Relaxed);
                        update_menu(hwnd);
                    }
                }
            }

            if reader.is_none() && last_open_try.elapsed() > Duration::from_millis(500) {
                reader = Reader::open();
                last_open_try = Instant::now();
            }
            let mut new_frame = false;
            if let Some(r) = reader.as_mut() {
                if let Some(s) = r.poll() {
                    snap = s;
                    new_frame = true;
                }
            }

            let stats = gfx.draw(&snap, &mut cache, view)?;
            if new_frame {
                fps_frames += 1;
            }
            perf.loops += 1;
            perf.build_ms += stats.build_ms;
            perf.present_ms += stats.present_ms;
            perf.worst_ms = perf.worst_ms.max(stats.build_ms + stats.present_ms);
            perf.draw_calls = perf.draw_calls.max(stats.draw_calls);
            if fps_clock.elapsed() >= Duration::from_secs(1) {
                if let Some(log) = stats_log.as_mut() {
                    use std::io::Write;
                    let n = perf.loops.max(1) as f32;
                    let (res, bb) = gfx.residual_share(&snap).unwrap_or((0.0, [0; 4]));
                    let _ = writeln!(log, "{:7.1}s frame {:6} received {:3} loops {:3} build {:5.1}ms present {:5.1}ms worst {:6.1}ms draws {:5} ops {:5} sprites {:4} lowres {:5.2}% at {},{}-{},{}",
                        start.elapsed().as_secs_f32(), snap.frame, fps_frames, perf.loops, perf.build_ms / n, perf.present_ms / n,
                        perf.worst_ms, perf.draw_calls, snap.back_ops.len(), snap.sprites.len(), res * 100.0, bb[0], bb[1], bb[2], bb[3]);
                    let _ = log.flush();
                }
                perf = Perf::default();
                fps = fps_frames as f32 / fps_clock.elapsed().as_secs_f32();
                fps_frames = 0;
                fps_clock = Instant::now();
                let (n, pending, bytes) = cache.stats();
                let look = match (view, cache.styles().get(cache.style)) {
                    (View::Hd, Some(style)) => style.name.as_str(),
                    _ => "original",
                };
                let status = if reader.is_none() { "waiting for game".to_string() } else { format!("frame {}", snap.frame) };
                let title = format!(
                    "Psycho Waluigi HD [{look}, F6 to switch] - {status} @ {fps:.0} fps - {}x{} - HD draws {} / low-res {} - {n} HD textures ({pending} loading, {} MB)\0",
                    snap.width * gfx.scale, snap.height * gfx.scale, stats.hd_draws, stats.fallback_draws, bytes >> 20);
                let title = controls_menu::capture_prompt().map(|p| p + "\0").unwrap_or(title);
                let wide: Vec<u16> = title.encode_utf16().collect();
                let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
            }
            if let Some(every) = args.auto_shot {
                if new_frame && last_auto.elapsed().as_secs_f32() >= every {
                    save_shots(&gfx, &snap, &args.shots);
                    last_auto = Instant::now();
                }
            }
            let _ = fps;
        }
    }
}
