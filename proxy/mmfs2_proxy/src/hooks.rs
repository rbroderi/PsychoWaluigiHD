//! Typed hooks for the mmfs2 sprite API (prototypes from the SDK's Cnpdll.h).
//!
//! Debug capture is off unless the MMFS2_CAPTURE environment variable is set (to anything) when
//! the game starts; it writes several MB a minute. With it on, output goes to <dll dir>/capture/:
//!   events.log   - sprite/image/backdrop events as they happen (ModifSpriteEx is only counted)
//!   frames.log   - full sprite list walked from the runtime at SpriteDraw: one frame every
//!                  PERIODIC_FRAMES, plus BURST_FRAMES consecutive frames after pressing F11
//!   screens/     - the logical screen surface (raw pixels) for each burst frame
//!   layout.log   - Spr struct layout check from the first AddSprite calls

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::thunks::ord;
use crate::{COUNTS, REAL};

const PERIODIC_FRAMES: u32 = 25;
const BURST_FRAMES: u32 = 50;
const VK_F11: i32 = 0x7A;

// Spr field offsets (Cnpdll.h), relative to the struct start once the Obj header is skipped.
const SPR_FLAGS: usize = 0;
const SPR_LAYER: usize = 4;
const SPR_ANGLE: usize = 6;
const SPR_ZORDER: usize = 8;
const SPR_X: usize = 12;
const SPR_Y: usize = 16;
const SPR_X1: usize = 20;
const SPR_Y1: usize = 24;
const SPR_X2: usize = 28;
const SPR_Y2: usize = 32;
const SPR_SCALE_X: usize = 76;
const SPR_SCALE_Y: usize = 80;
const SPR_IMG: usize = 96;
const SPR_EFFECT: usize = 104;
const SPR_EFFECT_PARAM: usize = 108;
const OBJ_HEADER: usize = 20;

const SF_HIDDEN: u32 = 0x80;
const SF_OWNERDRAW: u32 = 0x2000;
const GS_BACKGROUND: u32 = 1;
const LAYER_ALL: i32 = -1;

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(key: i32) -> i16;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// A capture log, or nowhere when capture is off.
type Log = BufWriter<Box<dyn Write + Send>>;

struct Capture {
    dir: PathBuf,
    events: Log,
    frames: Log,
    layout: Log,
    start: Instant,
    frame: u32,
    burst_left: u32,
    f11_down: bool,
    layout_checks: u32,
    /// Offset of the Spr fields inside the object: OBJ_HEADER unless the layout check says otherwise.
    base: usize,
    /// Image fingerprint (see tools/build_image_keys.py) -> original image-bank handle.
    image_keys: HashMap<String, u32>,
    /// mmfs2 image handle (as used by sprites) -> original image-bank handle.
    bank_of: HashMap<u32, u32>,
    /// mmfs2 image handle -> its size, so pastes carry their real rectangle.
    image_size: HashMap<u32, (i32, i32)>,
    matched: u32,
    unmatched: u32,
    /// Shared-memory frame feed for the HD viewer.
    publisher: Option<crate::publish::Publisher>,
    /// cSurface objects filled by LockImageSurface -> bank handle (quick-backdrop tiles).
    surf_bank: HashMap<usize, u32>,
    /// Off-screen surfaces -> the bank-image blits they hold, in the surface's own coordinates.
    surf_ops: HashMap<usize, Vec<hdshared::BackOp>>,
}

static CAPTURE: OnceLock<Mutex<Capture>> = OnceLock::new();
static MODIF_CALLS: AtomicU32 = AtomicU32::new(0);
/// Most recent window seen, used to recognise its screen/background surfaces as fill targets.
static LAST_WIN: AtomicUsize = AtomicUsize::new(0);
/// Set while the runtime's SpriteDraw runs: blits then are sprites and owner-drawn objects (text
/// boxes, counters), which are erased again every frame, not background.
static IN_SPRITE_DRAW: AtomicBool = AtomicBool::new(false);
/// MMFS2_NO_SURF_TRACK: the surface hooks only forward (no fill/blit tracking).
static NO_SURF_TRACK: AtomicBool = AtomicBool::new(false);
/// MMFS2_CAPTURE: write the capture logs, frame dumps and F11 screen bursts.
static LOGGING: AtomicBool = AtomicBool::new(false);

/// Whether debug capture (MMFS2_CAPTURE) is on.
pub fn logging() -> bool {
    LOGGING.load(Ordering::Relaxed)
}

pub fn init(dir: PathBuf) {
    let dir_for_masks = dir.clone();
    LOGGING.store(switch("MMFS2_CAPTURE"), Ordering::Relaxed);
    if logging() {
        let _ = fs::create_dir_all(dir.join("capture").join("screens"));
    }
    let image_keys = fs::read_to_string(dir.join("image_keys.tsv"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(k, h)| Some((k.to_string(), h.parse().ok()?)))
        .collect();
    let dir = dir.join("capture");
    let open = |name: &str| -> Log {
        let out: Box<dyn Write + Send> = match File::create(dir.join(name)) {
            Ok(f) if logging() => Box::new(f),
            _ => Box::new(io::sink()),
        };
        BufWriter::new(out)
    };
    let cap = Capture {
        events: open("events.log"),
        frames: open("frames.log"),
        layout: open("layout.log"),
        dir,
        start: Instant::now(),
        frame: 0,
        burst_left: 0,
        f11_down: false,
        layout_checks: 0,
        base: OBJ_HEADER,
        image_keys,
        bank_of: HashMap::new(),
        image_size: HashMap::new(),
        matched: 0,
        unmatched: 0,
        publisher: if switch("MMFS2_NO_PUBLISH") {
            None
        } else {
            crate::publish::Publisher::new().map(|mut p| {
                p.masks = crate::masks::Masks::load(&dir_for_masks.join("image_masks.bin"));
                p
            })
        },
        surf_bank: HashMap::new(),
        surf_ops: HashMap::new(),
    };
    let _ = CAPTURE.set(Mutex::new(cap));
    NO_SURF_TRACK.store(switch("MMFS2_NO_SURF_TRACK"), Ordering::Relaxed);
}

/// Debug switches, read once at load: set the environment variable to anything to enable.
fn switch(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn with_capture(f: impl FnOnce(&mut Capture)) {
    if let Some(m) = CAPTURE.get() {
        if let Ok(mut c) = m.lock() {
            f(&mut c);
        }
    }
}

macro_rules! event {
    ($($arg:tt)*) => {
        if logging() {
            with_capture(|c| {
                let ms = c.start.elapsed().as_millis();
                let frame = c.frame;
                let _ = writeln!(c.events, "{frame:>7} {ms:>8} {}", format_args!($($arg)*));
            })
        }
    };
}

fn bank_name(img: u32) -> String {
    let mut name = "?".to_string();
    with_capture(|c| {
        if let Some(b) = c.bank_of.get(&img) {
            name = b.to_string();
        }
    });
    name
}

unsafe fn real<F: Copy>(ordinal: usize) -> F {
    COUNTS[ordinal].fetch_add(1, Ordering::Relaxed);
    let addr = (*std::ptr::addr_of!(REAL))[ordinal];
    std::mem::transmute_copy(&addr)
}

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF29CE484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001B3))
}

/// Bytes of an mmfs2 RLE stream (16-bit pixels) needed to produce `pixels` pixels.
unsafe fn rle_length(data: usize, pixels: usize) -> usize {
    let (mut i, mut done) = (0usize, 0usize);
    while done < pixels {
        let n = *((data + i) as *const u8) as usize;
        i += 1;
        if n == 0 {
            break;
        }
        if n > 128 {
            i += (n - 128) * 2;
            done += n - 128;
        } else {
            i += 2;
            done += n;
        }
    }
    i
}

unsafe fn rd<T: Copy>(p: usize, off: usize) -> T {
    std::ptr::read_unaligned((p + off) as *const T)
}

// --- Sprite creation / modification ----------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn hook_AddSprite(
    win: usize, x: i32, y: i32, img: u32, layer: u32, z: i32, back: u32, flags: u32, extra: usize,
) -> usize {
    type F = unsafe extern "system" fn(usize, i32, i32, u32, u32, i32, u32, u32, usize) -> usize;
    let spr = real::<F>(ord::AddSprite)(win, x, y, img, layer, z, back, flags, extra);
    event!("AddSprite spr={spr:#x} win={win:#x} x={x} y={y} img={img} layer={} z={z} back={back:#x} flags={flags:#x}", layer & 0xFFFF);
    if spr != 0 {
        check_layout(spr, x, y, img, layer & 0xFFFF);
    }
    spr
}

#[no_mangle]
pub unsafe extern "system" fn hook_AddOwnerDrawSprite(
    win: usize, x1: i32, y1: i32, x2: i32, y2: i32, layer: u32, z: i32, back: u32, flags: u32,
    extra: usize, proc_: usize,
) -> usize {
    type F = unsafe extern "system" fn(usize, i32, i32, i32, i32, u32, i32, u32, u32, usize, usize) -> usize;
    let spr = real::<F>(ord::AddOwnerDrawSprite)(win, x1, y1, x2, y2, layer, z, back, flags, extra, proc_);
    event!("AddOwnerDrawSprite spr={spr:#x} rect={x1},{y1},{x2},{y2} layer={} z={z} flags={flags:#x} proc={proc_:#x}", layer & 0xFFFF);
    spr
}

#[no_mangle]
pub unsafe extern "system" fn hook_ModifSprite(win: usize, spr: usize, x: i32, y: i32, img: u32) -> usize {
    type F = unsafe extern "system" fn(usize, usize, i32, i32, u32) -> usize;
    MODIF_CALLS.fetch_add(1, Ordering::Relaxed);
    real::<F>(ord::ModifSprite)(win, spr, x, y, img)
}

#[no_mangle]
pub unsafe extern "system" fn hook_ModifSpriteEx(
    win: usize, spr: usize, x: i32, y: i32, img: u32, sx: f32, sy: f32, resample: i32, angle: i32,
    antia: i32,
) -> usize {
    type F = unsafe extern "system" fn(usize, usize, i32, i32, u32, f32, f32, i32, i32, i32) -> usize;
    MODIF_CALLS.fetch_add(1, Ordering::Relaxed);
    real::<F>(ord::ModifSpriteEx)(win, spr, x, y, img, sx, sy, resample, angle, antia)
}

#[no_mangle]
pub unsafe extern "system" fn hook_ModifSpriteEffect(win: usize, spr: usize, effect: u32, param: usize) -> usize {
    type F = unsafe extern "system" fn(usize, usize, u32, usize) -> usize;
    event!("ModifSpriteEffect spr={spr:#x} effect={effect:#x} param={param:#x}");
    real::<F>(ord::ModifSpriteEffect)(win, spr, effect, param)
}

#[no_mangle]
pub unsafe extern "system" fn hook_ModifOwnerDrawSprite(
    win: usize, spr: usize, x1: i32, y1: i32, x2: i32, y2: i32,
) -> usize {
    type F = unsafe extern "system" fn(usize, usize, i32, i32, i32, i32) -> usize;
    event!("ModifOwnerDrawSprite spr={spr:#x} rect={x1},{y1},{x2},{y2}");
    real::<F>(ord::ModifOwnerDrawSprite)(win, spr, x1, y1, x2, y2)
}

#[no_mangle]
pub unsafe extern "system" fn hook_ShowSprite(win: usize, spr: usize, show: i32) {
    type F = unsafe extern "system" fn(usize, usize, i32);
    event!("ShowSprite spr={spr:#x} show={show}");
    real::<F>(ord::ShowSprite)(win, spr, show)
}

#[no_mangle]
pub unsafe extern "system" fn hook_SetSpriteLayer(win: usize, spr: usize, layer: i32) {
    type F = unsafe extern "system" fn(usize, usize, i32);
    event!("SetSpriteLayer spr={spr:#x} layer={layer}");
    real::<F>(ord::SetSpriteLayer)(win, spr, layer)
}

// --- Deletion / ordering -----------------------------------------------------------------

macro_rules! win_spr_hook {
    ($name:ident, $ord:ident) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name(win: usize, spr: usize) {
            type F = unsafe extern "system" fn(usize, usize);
            event!(concat!(stringify!($ord), " spr={:#x}"), spr);
            real::<F>(ord::$ord)(win, spr)
        }
    };
}
win_spr_hook!(hook_DelSprite, DelSprite);
win_spr_hook!(hook_DelSpriteFast, DelSpriteFast);
win_spr_hook!(hook_MoveSpriteToFront, MoveSpriteToFront);
win_spr_hook!(hook_MoveSpriteToBack, MoveSpriteToBack);

macro_rules! win_spr_spr_hook {
    ($name:ident, $ord:ident) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name(win: usize, a: usize, b: usize) {
            type F = unsafe extern "system" fn(usize, usize, usize);
            event!(concat!(stringify!($ord), " spr={:#x} other={:#x}"), a, b);
            real::<F>(ord::$ord)(win, a, b)
        }
    };
}
win_spr_spr_hook!(hook_MoveSpriteBefore, MoveSpriteBefore);
win_spr_spr_hook!(hook_MoveSpriteAfter, MoveSpriteAfter);
win_spr_spr_hook!(hook_SwapSprites, SwapSprites);

#[no_mangle]
pub unsafe extern "system" fn hook_DelSprites(win: usize) {
    type F = unsafe extern "system" fn(usize);
    LAST_WIN.store(win, Ordering::Relaxed);
    event!("DelSprites win={win:#x}");
    with_capture(|c| {
        if let Some(p) = c.publisher.as_mut() {
            p.reset_back_ops();
        }
        // What off-screen surfaces hold belongs to the level being left. The transition copies
        // the old frame to the screen while the new level loads, which must not bring it back.
        c.surf_ops.clear();
    });
    real::<F>(ord::DelSprites)(win)
}

// --- Backdrops and images --------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn hook_PasteSprite(win: usize, img: u32, x: i32, y: i32, flags: u32) {
    type F = unsafe extern "system" fn(usize, u32, i32, i32, u32);
    let bank = bank_name(img); // outside event!, which holds the capture lock
    event!("PasteSprite img={img} bank={bank} x={x} y={y} flags={flags:#x}");
    record_paste(img, x, y, 0, 0);
    real::<F>(ord::PasteSprite)(win, img, x, y, flags)
}

#[no_mangle]
pub unsafe extern "system" fn hook_PasteSpriteEffect(
    win: usize, img: u32, x: i32, y: i32, flags: u32, effect: u32, param: usize,
) {
    type F = unsafe extern "system" fn(usize, u32, i32, i32, u32, u32, usize);
    let bank = bank_name(img); // outside event!, which holds the capture lock
    event!("PasteSpriteEffect img={img} bank={bank} x={x} y={y} flags={flags:#x} effect={effect:#x} param={param:#x}");
    record_paste(img, x, y, effect, param as u32);
    real::<F>(ord::PasteSpriteEffect)(win, img, x, y, flags, effect, param)
}

fn record_paste(img: u32, x: i32, y: i32, effect: u32, param: u32) {
    with_capture(|c| {
        let bank = c.bank_of.get(&img).copied().unwrap_or(hdshared::NO_BANK);
        // The image's real size, so a small paste at the same corner as a big one doesn't
        // count as replacing it.
        let (w, h) = c.image_size.get(&img).copied().unwrap_or((0, 0));
        if let Some(p) = c.publisher.as_mut() {
            p.add_back_op(hdshared::BackOp { kind: hdshared::OP_PASTE, bank, x, y, w, h, effect, param, ..Default::default() }, "paste");
        }
    });
}

#[no_mangle]
pub unsafe extern "system" fn hook_AddImage(
    app: usize, w: u32, h: u32, xs: u32, ys: u32, xa: u32, ya: u32, trsp: u32, mode: u32,
    data: usize, extra: usize,
) -> u32 {
    type F = unsafe extern "system" fn(usize, u32, u32, u32, u32, u32, u32, u32, u32, usize, usize) -> u32;
    let handle = real::<F>(ord::AddImage)(app, w, h, xs, ys, xa, ya, trsp, mode, data, extra);
    let peek = |p: usize| -> String {
        if p < 0x10000 {
            return "-".into();
        }
        (0..32).map(|i| format!("{:02x}", *((p + i) as *const u8))).collect()
    };
    let (w, h) = (w & 0xFFFF, h & 0xFFFF);
    let (xs, ys, xa, ya) = (xs as u16 as i16, ys as u16 as i16, xa as u16 as i16, ya as u16 as i16);
    let key = if data >= 0x10000 && w > 0 && h > 0 {
        let rle = (mode >> 8) & 0x02 != 0;
        let len = if rle { rle_length(data, (w * h) as usize) } else { (w * h * 2) as usize };
        let bytes = std::slice::from_raw_parts(data as *const u8, len);
        Some(format!("{w} {h} {xs} {ys} {xa} {ya} {:016x}", fnv1a64(bytes)))
    } else {
        None
    };
    let mut bank = None;
    with_capture(|c| {
        bank = key.as_ref().and_then(|k| c.image_keys.get(k).copied());
        c.image_size.insert(handle, (w as i32, h as i32));
        match bank {
            Some(b) => {
                c.bank_of.insert(handle, b);
                c.matched += 1;
            }
            None => {
                c.bank_of.remove(&handle);
                c.unmatched += 1;
            }
        }
    });
    let bank = bank.map_or("?".to_string(), |b| b.to_string());
    event!(
        "AddImage -> {handle} bank={bank} size={w}x{h} hot={xs},{ys} act={xa},{ya} trsp={trsp:#x} mode={mode:#x} data={data:#x}:{}",
        peek(data)
    );
    handle
}

#[no_mangle]
pub unsafe extern "system" fn hook_DelImage(app: usize, img: u32) -> i32 {
    type F = unsafe extern "system" fn(usize, u32) -> i32;
    event!("DelImage {img}");
    with_capture(|c| {
        c.bank_of.remove(&img);
    });
    real::<F>(ord::DelImage)(app, img)
}

#[no_mangle]
pub unsafe extern "system" fn hook_WinGetLogRect(win: usize, rc: *mut Rect) {
    type F = unsafe extern "system" fn(usize, *mut Rect);
    real::<F>(ord::WinGetLogRect)(win, rc)
}

// --- Frame capture -------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn hook_SpriteDraw(win: usize) {
    type F = unsafe extern "system" fn(usize);
    IN_SPRITE_DRAW.store(true, Ordering::Relaxed);
    real::<F>(ord::SpriteDraw)(win);
    IN_SPRITE_DRAW.store(false, Ordering::Relaxed);
    LAST_WIN.store(win, Ordering::Relaxed);
    if let Some(msg) = crate::guard::install() {
        event!("{msg}");
    }
    crate::guard::flush_pending();
    let deferred = crate::guard::DEFERRED.swap(0, Ordering::Relaxed);
    if deferred > 0 {
        event!("pause guard: deferred {deferred} Pause command(s) from a transition");
    }
    with_capture(|c| {
        c.frame += 1;
        let (frame, base) = (c.frame, c.base);
        if let Some(p) = c.publisher.as_mut() {
            let bank_of = &c.bank_of;
            p.publish(win, frame, base, &|img| bank_of.get(&img).copied());
        }
        if !logging() {
            return;
        }
        let f11 = GetAsyncKeyState(VK_F11) as u16 & 0x8000 != 0;
        if f11 && !c.f11_down {
            c.burst_left = BURST_FRAMES;
            let frame = c.frame;
            let _ = writeln!(c.events, "{frame:>7} F11 burst start");
        }
        c.f11_down = f11;
        let burst = c.burst_left > 0;
        if burst {
            c.burst_left -= 1;
        }
        if burst || c.frame % PERIODIC_FRAMES == 0 {
            dump_frame(c, win);
            if burst {
                dump_screen(c, win);
            }
            if !burst || c.burst_left == 0 {
                let _ = c.frames.flush();
                let _ = c.events.flush();
            }
        }
    });
}

unsafe fn dump_frame(c: &mut Capture, win: usize) {
    type LogRect = unsafe extern "system" fn(usize, *mut Rect);
    let mut rc = Rect::default();
    // Called through REAL directly so our own calls don't show up in the counts.
    let addr = (*std::ptr::addr_of!(REAL))[ord::WinGetLogRect];
    std::mem::transmute::<usize, LogRect>(addr)(win, &mut rc);
    let ms = c.start.elapsed().as_millis();
    let modifs = MODIF_CALLS.swap(0, Ordering::Relaxed);
    let _ = writeln!(
        c.frames,
        "F {} t={ms} win={win:#x} log={},{},{},{} modif_calls={modifs} images_matched={} unmatched={}",
        c.frame, rc.left, rc.top, rc.right, rc.bottom, c.matched, c.unmatched
    );
    if let Some(p) = c.publisher.as_ref() {
        for (op, frame, source) in p.debug_ops() {
            let _ = writeln!(c.frames, "O {source} k={} bank={} at={},{} size={}x{} org={},{} fx={:#x} drawn_frame={frame}",
                             op.kind, op.bank, op.x, op.y, op.w, op.h, op.org_x, op.org_y, op.effect);
        }
    }
    for (kind, flags) in [("S", 0u32), ("B", GS_BACKGROUND)] {
        type First = unsafe extern "system" fn(usize, i32, u32) -> usize;
        type Next = unsafe extern "system" fn(usize, usize, u32) -> usize;
        let table = &*std::ptr::addr_of!(REAL);
        let first: First = std::mem::transmute(table[ord::GetFirstSprite]);
        let next: Next = std::mem::transmute(table[ord::GetNextSprite]);
        let mut spr = first(win, LAYER_ALL, flags);
        let mut guard = 0;
        while spr != 0 && guard < 10_000 {
            write_sprite(c, kind, spr);
            spr = next(win, spr, flags);
            guard += 1;
        }
    }
}

unsafe fn write_sprite(c: &mut Capture, kind: &str, spr: usize) {
    let b = spr + c.base;
    let flags: u32 = rd(b, SPR_FLAGS);
    let img: u32 = rd(b, SPR_IMG);
    let img = if flags & SF_OWNERDRAW != 0 {
        format!("od:{img:#x}")
    } else {
        match c.bank_of.get(&img) {
            Some(b) => format!("{img} bank={b}"),
            None => format!("{img} bank=?"),
        }
    };
    let _ = writeln!(
        c.frames,
        "{kind} {spr:#x} fl={flags:#x}{} layer={} z={} img={img} pos={},{} box={},{},{},{} scale={},{} angle={} fx={:#x}/{:#x}",
        if flags & SF_HIDDEN != 0 { " HIDDEN" } else { "" },
        rd::<u16>(b, SPR_LAYER), rd::<i32>(b, SPR_ZORDER),
        rd::<i32>(b, SPR_X), rd::<i32>(b, SPR_Y),
        rd::<i32>(b, SPR_X1), rd::<i32>(b, SPR_Y1), rd::<i32>(b, SPR_X2), rd::<i32>(b, SPR_Y2),
        rd::<f32>(b, SPR_SCALE_X), rd::<f32>(b, SPR_SCALE_Y), rd::<i16>(b, SPR_ANGLE),
        rd::<u32>(b, SPR_EFFECT), rd::<u32>(b, SPR_EFFECT_PARAM),
    );
}

/// Writes the logical screen as screens/f<frame>.bin: a 16-byte header
/// (width, height, depth, pitch as little-endian i32) then `height` rows of |pitch| bytes.
unsafe fn dump_screen(c: &mut Capture, win: usize) {
    let table = &*std::ptr::addr_of!(REAL);
    let get_surface: unsafe extern "system" fn(usize, i32) -> usize = std::mem::transmute(table[ord::WinGetSurface]);
    let surf = get_surface(win, 0);
    if surf == 0 {
        return;
    }
    let getter = |o: usize| -> i32 {
        let f: unsafe extern "thiscall" fn(usize) -> i32 = std::mem::transmute(table[o]);
        f(surf)
    };
    let (w, h, depth, pitch) = (getter(ord::SurfGetWidth), getter(ord::SurfGetHeight),
                                getter(ord::SurfGetDepth), getter(ord::SurfGetPitch));
    let lock: unsafe extern "thiscall" fn(usize) -> *mut u8 = std::mem::transmute(table[ord::SurfLockBuffer]);
    let unlock: unsafe extern "thiscall" fn(usize, *mut u8) = std::mem::transmute(table[ord::SurfUnlockBuffer]);
    let buf = lock(surf);
    if buf.is_null() || w <= 0 || h <= 0 {
        return;
    }
    let row = pitch.unsigned_abs() as usize;
    let mut out = Vec::with_capacity(16 + row * h as usize);
    for v in [w, h, depth, pitch] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for y in 0..h as isize {
        let p = buf.offset(y * pitch as isize);
        out.extend_from_slice(std::slice::from_raw_parts(p, row));
    }
    unlock(surf, buf);
    let _ = fs::write(c.dir.join("screens").join(format!("f{:07}.bin", c.frame)), out);
}

/// Logs the raw Spr struct for the first few sprites and works out where the SDK fields start,
/// by finding the offset at which the x/y/image/layer passed to AddSprite show up.
unsafe fn check_layout(spr: usize, x: i32, y: i32, img: u32, layer: u32) {
    with_capture(|c| {
        if c.layout_checks >= 20 {
            return;
        }
        c.layout_checks += 1;
        let words: Vec<String> = (0..40).map(|i| format!("{:08x}", rd::<u32>(spr, i * 4))).collect();
        let _ = writeln!(c.layout, "spr={spr:#x} x={x} y={y} img={img} layer={layer}\n  {}", words.join(" "));
        for base in [OBJ_HEADER, 0] {
            let ok_pos = rd::<i32>(spr + base, SPR_X) == x && rd::<i32>(spr + base, SPR_Y) == y;
            let ok_img = rd::<u32>(spr + base, SPR_IMG) == img;
            let ok_layer = rd::<u16>(spr + base, SPR_LAYER) as u32 == layer;
            let _ = writeln!(c.layout, "  base {base}: pos={ok_pos} img={ok_img} layer={ok_layer}");
            if ok_img && ok_layer && base != c.base {
                c.base = base;
                let _ = writeln!(c.layout, "  -> switching base to {base}");
            }
        }
        let _ = c.layout.flush();
    });
}


// --- Quick backdrops: rectangles filled with a tile, gradient or flat colour ---------------------

#[no_mangle]
pub unsafe extern "system" fn hook_LockImageSurface(app: usize, img: u32, surf: usize, mode: i32) -> i32 {
    type F = unsafe extern "system" fn(usize, u32, usize, i32) -> i32;
    let ok = real::<F>(ord::LockImageSurface)(app, img, surf, mode);
    with_capture(|c| {
        let bank = c.bank_of.get(&img).copied();
        match bank {
            Some(b) if ok != 0 => {
                c.surf_bank.insert(surf, b);
            }
            _ => {
                c.surf_bank.remove(&surf);
            }
        }
        let frame = c.frame;
        let _ = writeln!(c.events, "{frame:>7} LockImageSurface img={img} bank={bank:?} surf={surf:#x} mode={mode} ok={ok}");
    });
    ok
}

#[no_mangle]
pub unsafe extern "system" fn hook_UnlockImageSurface(surf: usize) {
    type F = unsafe extern "system" fn(usize);
    with_capture(|c| {
        c.surf_bank.remove(&surf);
    });
    real::<F>(ord::UnlockImageSurface)(surf)
}

/// Which window surface `surf` is: 0 = logical screen, 1 = background save, None = something else.
unsafe fn window_surface_id(surf: usize) -> Option<i32> {
    let win = LAST_WIN.load(Ordering::Relaxed);
    if win == 0 {
        return None;
    }
    let get = crate::publish::call::<unsafe extern "system" fn(usize, i32) -> usize>(ord::WinGetSurface);
    (0..2).find(|&id| get(win, id) == surf)
}

/// Turns a rectangle fill into a background op, if the fill type is one we can redraw.
unsafe fn record_fill(dest: usize, l: i32, t: i32, r: i32, b: i32, fill: usize, bo: u32, param: usize) {
    if NO_SURF_TRACK.load(Ordering::Relaxed) {
        return;
    }
    let target = window_surface_id(dest);
    let vtbl = if fill != 0 { *(fill as *const usize) } else { 0 };
    let data = &*std::ptr::addr_of!(crate::REAL_DATA);
    let field = |i: usize| *((fill + 4 * i) as *const u32);
    with_capture(|c| {
        let mut op = hdshared::BackOp {
            bank: hdshared::NO_BANK, x: l, y: t, w: r - l, h: b - t, effect: bo, param: param as u32,
            ..Default::default()
        };
        let kind = if vtbl == data[ord::VtblFillMosaic] {
            // CFillMosaic: vptr, m_pSfTile, m_xOrg, m_yOrg
            op.kind = hdshared::OP_TILE;
            op.bank = c.surf_bank.get(&(field(1) as usize)).copied().unwrap_or(hdshared::NO_BANK);
            op.org_x = field(2) as i32;
            op.org_y = field(3) as i32;
            "tile"
        } else if vtbl == data[ord::VtblFillDir] {
            // CFillDir: vptr, m_xOrg, m_yOrg, m_crFrom, m_crTo, m_vert
            op.kind = hdshared::OP_GRADIENT;
            op.org_x = field(1) as i32;
            op.org_y = field(2) as i32;
            op.color1 = field(3);
            op.color2 = field(4);
            op.vertical = field(5);
            "gradient"
        } else if vtbl == data[ord::VtblFillFlat] {
            // CFillFlat: vptr, m_cr
            op.kind = hdshared::OP_FLAT;
            op.color1 = field(1);
            "flat"
        } else {
            "unknown"
        };
        let frame = c.frame;
        let _ = writeln!(c.events, "{frame:>7} RectFill {kind} target={target:?} rect={l},{t},{r},{b} bank={} org={},{} colors={:#x},{:#x} vert={} bo={bo} param={param:#x}",
                         op.bank, op.org_x, op.org_y, op.color1, op.color2, op.vertical);
        // Fills into the background save surface are level backdrops; ones straight onto the
        // screen are redrawn every frame and already show in the low-res screen layer.
        if kind != "unknown" && target == Some(1) && (op.kind != hdshared::OP_TILE || op.bank != hdshared::NO_BANK) {
            if let Some(p) = c.publisher.as_mut() {
                p.add_back_op(op, "fill");
            }
        }
    });
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfRectFill(
    surf: usize, l: i32, t: i32, r: i32, b: i32, fill: usize, antia: i32, bm: u32, bo: u32, param: usize,
) -> i32 {
    type F = unsafe extern "thiscall" fn(usize, i32, i32, i32, i32, usize, i32, u32, u32, usize) -> i32;
    record_fill(surf, l, t, r, b, fill, bo, param);
    real::<F>(ord::SurfRectFill)(surf, l, t, r, b, fill, antia, bm, bo, param)
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfRectFillOutline(
    surf: usize, l: i32, t: i32, r: i32, b: i32, fill: usize, thickness: i32, outline: usize, antia: i32,
    bm: u32, bo: u32, param: usize, flag: i32,
) -> i32 {
    type F = unsafe extern "thiscall" fn(usize, i32, i32, i32, i32, usize, i32, usize, i32, u32, u32, usize, i32) -> i32;
    if fill != 0 && thickness == 0 {
        record_fill(surf, l, t, r, b, fill, bo, param);
    } else {
        event!("RectFillOutline thickness={thickness} fill={fill:#x} outline={outline:#x} rect={l},{t},{r},{b}");
    }
    real::<F>(ord::SurfRectFillOutline)(surf, l, t, r, b, fill, thickness, outline, antia, bm, bo, param, flag)
}


// --- Background blits ------------------------------------------------------------------------------
// Backdrops reach the window as bank-image blits (quick backdrops lock the tile image with
// LockImageSurface and blit it tile by tile): when a level loads, when scrolling exposes a new
// strip, and every frame for animated backdrops. Every such blit onto the window's screen or
// background surface outside SpriteDraw becomes a background op; WinScroll moves them along.
// Image content blitted into off-screen surfaces is followed too: each surface remembers the
// bank blits it holds, and a blit from it into a window surface emits them (moved and clipped).

/// Ops a blit of `src`'s rectangle (sx, sy, sw, sh) to (dx, dy) carries, in destination coordinates.
fn blit_ops(c: &Capture, src: usize, dx: i32, dy: i32, sx: i32, sy: i32, sw: i32, sh: i32, bo: u32, param: usize)
    -> Vec<hdshared::BackOp> {
    if let Some(&bank) = c.surf_bank.get(&src) {
        return vec![hdshared::BackOp {
            kind: hdshared::OP_BLIT, bank, x: dx, y: dy, w: sw, h: sh, org_x: sx, org_y: sy,
            effect: bo, param: param as u32, ..Default::default()
        }];
    }
    let Some(inner) = c.surf_ops.get(&src) else { return Vec::new() };
    inner.iter().filter_map(|op| {
        // Clip the held op to the copied rectangle, then move it to the destination.
        let (x1, y1) = (op.x.max(sx), op.y.max(sy));
        let (x2, y2) = ((op.x + op.w).min(sx + sw), (op.y + op.h).min(sy + sh));
        if x2 <= x1 || y2 <= y1 {
            return None;
        }
        let effect = if op.effect & hdshared::EFFECT_MASK == hdshared::BOP_COPY { bo } else { op.effect };
        Some(hdshared::BackOp {
            x: x1 - sx + dx, y: y1 - sy + dy, w: x2 - x1, h: y2 - y1,
            org_x: op.org_x + (x1 - op.x), org_y: op.org_y + (y1 - op.y),
            effect, param: if effect == bo { param as u32 } else { op.param },
            ..*op
        })
    }).collect()
}

unsafe fn track_blit(src: usize, dest: usize, dx: i32, dy: i32, sx: i32, sy: i32, sw: i32, sh: i32, bo: u32, param: usize) {
    if NO_SURF_TRACK.load(Ordering::Relaxed) {
        return;
    }
    // Cheap exit for the common case: neither a locked image nor a surface holding image blits.
    let mut relevant = false;
    with_capture(|c| relevant = c.surf_bank.contains_key(&src) || c.surf_ops.contains_key(&src));
    if !relevant {
        return;
    }
    let target = window_surface_id(dest);
    let in_sprite_draw = IN_SPRITE_DRAW.load(Ordering::Relaxed);
    with_capture(|c| {
        let ops = blit_ops(c, src, dx, dy, sx, sy, sw, sh, bo, param);
        if ops.is_empty() {
            return;
        }
        match target {
            // Backdrops, drawn when a level loads, when scrolling exposes a new strip, or every
            // frame for animated ones: all onto the window's screen or background surface.
            Some(_) if !in_sprite_draw => {
                let source = if c.surf_bank.contains_key(&src) { "blit" } else { "composite" };
                if let Some(p) = c.publisher.as_mut() {
                    for op in ops {
                        p.add_back_op(op, source);
                    }
                }
            }
            Some(_) => {}
            None => {
                // What the new blit covers replaces what was there.
                let (x1, y1, x2, y2) = (dx, dy, dx + sw, dy + sh);
                let held = c.surf_ops.entry(dest).or_default();
                held.retain(|o| !(o.x >= x1 && o.y >= y1 && o.x + o.w <= x2 && o.y + o.h <= y2));
                held.extend(ops)
            }
        }
    });
}

fn forget_surface(surf: usize) {
    with_capture(|c| {
        c.surf_ops.remove(&surf);
    });
}

unsafe fn surface_size(surf: usize) -> (i32, i32) {
    let get = |o: usize| crate::publish::call::<unsafe extern "thiscall" fn(usize) -> i32>(o)(surf);
    (get(ord::SurfGetWidth), get(ord::SurfGetHeight))
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfBlitRect(
    src: usize, dest: usize, dx: i32, dy: i32, sx: i32, sy: i32, sw: i32, sh: i32, bm: u32, bo: u32,
    param: usize, flags: u32,
) -> i32 {
    type F = unsafe extern "thiscall" fn(usize, usize, i32, i32, i32, i32, i32, i32, u32, u32, usize, u32) -> i32;
    track_blit(src, dest, dx, dy, sx, sy, sw, sh, bo, param);
    real::<F>(ord::SurfBlitRect)(src, dest, dx, dy, sx, sy, sw, sh, bm, bo, param, flags)
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfBlitAt(
    src: usize, dest: usize, dx: i32, dy: i32, bm: u32, bo: u32, param: usize, flags: u32,
) -> i32 {
    type F = unsafe extern "thiscall" fn(usize, usize, i32, i32, u32, u32, usize, u32) -> i32;
    let (w, h) = surface_size(src);
    track_blit(src, dest, dx, dy, 0, 0, w, h, bo, param);
    real::<F>(ord::SurfBlitAt)(src, dest, dx, dy, bm, bo, param, flags)
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfBlitAll(src: usize, dest: usize) -> i32 {
    type F = unsafe extern "thiscall" fn(usize, usize) -> i32;
    let (w, h) = surface_size(src);
    track_blit(src, dest, 0, 0, 0, 0, w, h, hdshared::BOP_COPY, 0);
    real::<F>(ord::SurfBlitAll)(src, dest)
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfCreate(surf: usize, w: i32, h: i32, proto: usize) {
    type F = unsafe extern "thiscall" fn(usize, i32, i32, usize);
    forget_surface(surf);
    real::<F>(ord::SurfCreate)(surf, w, h, proto)
}

#[no_mangle]
pub unsafe extern "thiscall" fn hook_SurfFillColor(surf: usize, color: u32) -> i32 {
    type F = unsafe extern "thiscall" fn(usize, u32) -> i32;
    forget_surface(surf);
    real::<F>(ord::SurfFillColor)(surf, color)
}

#[no_mangle]
pub unsafe extern "system" fn hook_DeleteSurface(surf: usize) {
    type F = unsafe extern "system" fn(usize);
    forget_surface(surf);
    real::<F>(ord::DeleteSurface)(surf)
}

#[no_mangle]
pub unsafe extern "system" fn hook_WinAttachSurface(win: usize, surf: usize) -> i32 {
    type F = unsafe extern "system" fn(usize, usize) -> i32;
    let r = real::<F>(ord::WinAttachSurface)(win, surf);
    event!("WinAttachSurface win={win:#x} surf={surf:#x} -> {r}");
    r
}

// --- Scrolling ---------------------------------------------------------------------------------------

/// WinScroll(win, xDest, yDest, xSrc, ySrc, width, height) moves the window's picture by
/// (xDest - xSrc, yDest - ySrc); the background ops move with it.
#[no_mangle]
pub unsafe extern "system" fn hook_WinScroll(win: usize, xd: i32, yd: i32, xs: i32, ys: i32, w: i32, h: i32) -> i32 {
    type F = unsafe extern "system" fn(usize, i32, i32, i32, i32, i32, i32) -> i32;
    let r = real::<F>(ord::WinScroll)(win, xd, yd, xs, ys, w, h);
    with_capture(|c| {
        if let Some(p) = c.publisher.as_mut() {
            p.scroll_back_ops(xd - xs, yd - ys);
        }
    });
    r
}

