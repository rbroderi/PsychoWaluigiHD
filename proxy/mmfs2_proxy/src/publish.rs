//! Publishes each frame to the HD viewer through the shared [`hdshared::Frame`] block.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use hdshared::{BackOp, Frame, Sprite, FRAME_SIZE, KIND_IMAGE, KIND_OWNERDRAW, MAX_BACK_OPS, MAX_SCREEN_H,
               MAX_SCREEN_W, MAX_SPRITES, NO_BANK};

use crate::thunks::ord;
use crate::REAL;

const PAGE_READWRITE: u32 = 4;
const FILE_MAP_ALL_ACCESS: u32 = 0xF001F;
const INVALID_HANDLE_VALUE: isize = -1;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(file: isize, attrs: usize, protect: u32, size_hi: u32, size_lo: u32, name: *const u16) -> isize;
    fn MapViewOfFile(map: isize, access: u32, off_hi: u32, off_lo: u32, bytes: usize) -> *mut u8;
}

pub struct Publisher {
    frame_block: *mut Frame,
    /// Size of the window's screen, for dropping ops that can't be seen.
    screen: (i32, i32),
    /// Background ops in drawing order, each with the frame it was drawn in and where it came from.
    back_ops: Vec<(BackOp, u32, &'static str)>,
    epoch: u32,
    /// Frame counter for tagging ops (advances at each publish).
    frame: u32,
    /// Opacity of the bank images, for pruning hidden ops.
    pub masks: Option<crate::masks::Masks>,
    /// Ops were added or moved since the last pruning.
    dirty: bool,
}

unsafe impl Send for Publisher {}

impl Publisher {
    pub fn new() -> Option<Publisher> {
        let name: Vec<u16> = hdshared::MAP_NAME.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let map = CreateFileMappingW(INVALID_HANDLE_VALUE, 0, PAGE_READWRITE, 0, FRAME_SIZE as u32, name.as_ptr());
            if map == 0 {
                return None;
            }
            let view = MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, FRAME_SIZE) as *mut Frame;
            if view.is_null() {
                return None;
            }
            (*view).header.magic = hdshared::MAGIC;
            (*view).header.version = hdshared::VERSION;
            Some(Publisher { frame_block: view, screen: (320, 240), back_ops: Vec::new(), epoch: 0, frame: 0, masks: None, dirty: false })
        }
    }

    /// Background ops model what is painted on the window's screen: they accumulate until the
    /// level changes, but drawing the same rectangle again (redraws, animation frames, counters)
    /// replaces the old op,
    /// and ops that newer ones completely hide are dropped (see `drop_hidden_ops`).
    /// Ops that can't be seen are dropped.
    pub fn add_back_op(&mut self, op: BackOp, source: &'static str) {
        if !self.visible(&op) {
            return;
        }
        self.back_ops.retain(|(q, _, _)| !(q.x == op.x && q.y == op.y && q.w == op.w && q.h == op.h));
        if self.back_ops.len() == MAX_BACK_OPS {
            self.back_ops.remove(0);
        }
        self.back_ops.push((op, self.frame, source));
        self.dirty = true;
    }

    /// The current ops with their frame and source, for the capture log.
    pub fn debug_ops(&self) -> &[(BackOp, u32, &'static str)] {
        &self.back_ops
    }

    /// Drops ops that newer ops completely hide. Walking from the newest op back, the opaque
    /// pixels of every fully opaque op (plain copy ink, known image) are marked covered; an
    /// older op none of whose opaque pixels is still uncovered can't be seen and goes. Scrolling
    /// levels repaint their backdrops every few frames at positions that rarely line up with
    /// the old pieces, so without this the list grows without bound; using the images' real
    /// opacity (not their rectangles) keeps what shows through transparent parts.
    fn drop_hidden_ops(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(masks) = self.masks.as_ref() else { return };
        let (w, h) = self.screen;
        let mut covered = vec![false; (w * h).max(0) as usize];
        let mut keep = vec![true; self.back_ops.len()];
        for (i, (op, _, _)) in self.back_ops.iter().enumerate().rev() {
            let solid_ink = op.effect & hdshared::EFFECT_MASK == hdshared::BOP_COPY;
            // Image ops: which image pixel lands on screen pixel (X, Y).
            let (mask, ox, oy) = match op.kind {
                hdshared::OP_PASTE | hdshared::OP_BLIT => match masks.get(op.bank) {
                    Some(m) => (Some(m), op.org_x - op.x, op.org_y - op.y),
                    None => continue, // unknown image: keep it, hides nothing
                },
                hdshared::OP_FLAT | hdshared::OP_GRADIENT => (None, 0, 0),
                _ => continue,
            };
            if op.w <= 0 || op.h <= 0 {
                continue;
            }
            let mut visible = false;
            for y in op.y.max(0)..(op.y + op.h).min(h) {
                for x in op.x.max(0)..(op.x + op.w).min(w) {
                    if let Some(m) = mask {
                        if !masks.opaque(m, x + ox, y + oy) {
                            continue;
                        }
                    }
                    let c = &mut covered[(y * w + x) as usize];
                    if !*c {
                        visible = true;
                        if solid_ink {
                            *c = true;
                        }
                    }
                }
            }
            keep[i] = visible;
        }
        let mut k = keep.into_iter();
        self.back_ops.retain(|_| k.next().unwrap_or(true));
    }

    /// The window's picture moved by (dx, dy).
    pub fn scroll_back_ops(&mut self, dx: i32, dy: i32) {
        for (op, _, _) in self.back_ops.iter_mut() {
            op.x += dx;
            op.y += dy;
        }
        let ops = std::mem::take(&mut self.back_ops);
        self.back_ops = ops.into_iter().filter(|(op, _, _)| self.visible(op)).collect();
        self.dirty = true;
    }

    /// Whether `op` can touch the screen. An op of unknown size gets a generous margin.
    fn visible(&self, op: &BackOp) -> bool {
        let (sw, sh) = self.screen;
        let (w, h) = if op.w > 0 && op.h > 0 { (op.w, op.h) } else { (1024, 1024) };
        op.x < sw && op.y < sh && op.x + w > 0 && op.y + h > 0
    }

    pub fn reset_back_ops(&mut self) {
        self.back_ops.clear();
        self.epoch += 1;
    }

    /// `bank_of` maps mmfs2 image handles to bank handles; `base` is the Spr field offset.
    pub unsafe fn publish(&mut self, win: usize, frame_no: u32, base: usize, bank_of: &dyn Fn(u32) -> Option<u32>) {
        self.drop_hidden_ops();
        let f = &mut *self.frame_block;
        let h = &mut f.header;
        h.seq.fetch_add(1, Ordering::AcqRel); // odd: writing

        let mut rc = [0i32; 4];
        call::<unsafe extern "system" fn(usize, *mut i32)>(ord::WinGetLogRect)(win, rc.as_mut_ptr());
        h.frame = frame_no;
        h.origin_x = rc[0];
        h.origin_y = rc[1];

        // Sprites: active and background lists, de-duplicated, back to front.
        let mut list: Vec<Sprite> = Vec::new();
        let mut seen = HashSet::new();
        for flags in [0u32, 1] {
            let mut spr = call::<unsafe extern "system" fn(usize, i32, u32) -> usize>(ord::GetFirstSprite)(win, -1, flags);
            while spr != 0 && list.len() < MAX_SPRITES {
                if seen.insert(spr) {
                    list.push(read_sprite(spr + base, bank_of));
                }
                spr = call::<unsafe extern "system" fn(usize, usize, u32) -> usize>(ord::GetNextSprite)(win, spr, flags);
            }
        }
        list.sort_by_key(|s| (s.layer, s.z));
        f.sprites[..list.len()].copy_from_slice(&list);
        h.n_sprites = list.len() as u32;

        for (dst, (op, _, _)) in f.back_ops.iter_mut().zip(&self.back_ops) {
            *dst = *op;
        }
        h.n_back_ops = self.back_ops.len() as u32;
        self.frame = self.frame.wrapping_add(1);
        h.back_epoch = self.epoch;

        match copy_surface(win, 0, &mut f.screen) {
            Some((w, hgt)) => {
                h.screen_w = w;
                h.screen_h = hgt;
                self.screen = (w as i32, hgt as i32);
                h.has_back = copy_surface(win, 1, &mut f.back).map_or(0, |d| (d == (w, hgt)) as u32);
            }
            None => {
                h.screen_w = 0;
                h.screen_h = 0;
                h.has_back = 0;
            }
        }

        h.seq.fetch_add(1, Ordering::AcqRel); // even: complete
    }
}

pub unsafe fn call<F: Copy>(ordinal: usize) -> F {
    let addr = (*std::ptr::addr_of!(REAL))[ordinal];
    std::mem::transmute_copy(&addr)
}

unsafe fn rd<T: Copy>(p: usize, off: usize) -> T {
    std::ptr::read_unaligned((p + off) as *const T)
}

unsafe fn read_sprite(b: usize, bank_of: &dyn Fn(u32) -> Option<u32>) -> Sprite {
    let flags: u32 = rd(b, 0);
    let owner_draw = flags & 0x2000 != 0;
    let img: u32 = rd(b, 96);
    Sprite {
        kind: if owner_draw { KIND_OWNERDRAW } else { KIND_IMAGE },
        bank: if owner_draw { NO_BANK } else { bank_of(img).unwrap_or(NO_BANK) },
        x: rd(b, 12),
        y: rd(b, 16),
        x1: rd(b, 20),
        y1: rd(b, 24),
        x2: rd(b, 28),
        y2: rd(b, 32),
        scale_x: rd(b, 76),
        scale_y: rd(b, 80),
        angle: rd::<i16>(b, 6) as i32,
        effect: rd(b, 104),
        param: rd(b, 108),
        layer: rd::<u16>(b, 4) as u32,
        z: rd(b, 8),
        flags,
    }
}

/// Copies window surface `id` (0 = logical screen, 1 = background save) as 0x00RRGGBB, top-down.
unsafe fn copy_surface(win: usize, id: i32, out: &mut [u32]) -> Option<(u32, u32)> {
    let surf = call::<unsafe extern "system" fn(usize, i32) -> usize>(ord::WinGetSurface)(win, id);
    if surf == 0 {
        return None;
    }
    let get = |o: usize| call::<unsafe extern "thiscall" fn(usize) -> i32>(o)(surf);
    let (w, h, depth, pitch) = (get(ord::SurfGetWidth), get(ord::SurfGetHeight), get(ord::SurfGetDepth), get(ord::SurfGetPitch));
    if depth != 32 || w <= 0 || h <= 0 || w as usize > MAX_SCREEN_W || h as usize > MAX_SCREEN_H {
        return None;
    }
    let buf = call::<unsafe extern "thiscall" fn(usize) -> *mut u8>(ord::SurfLockBuffer)(surf);
    if buf.is_null() {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    for y in 0..h {
        let row = buf.offset(y as isize * pitch as isize) as *const u32;
        for x in 0..w {
            out[y * w + x] = *row.add(x) & 0x00FF_FFFF;
        }
    }
    call::<unsafe extern "thiscall" fn(usize, *mut u8)>(ord::SurfUnlockBuffer)(surf, buf);
    Some((w as u32, h as u32))
}

