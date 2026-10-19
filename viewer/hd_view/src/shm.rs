//! Reader side of the shared frame block written by the mmfs2 proxy.

use std::sync::atomic::Ordering;

use hdshared::{BackOp, Frame, Header, Sprite, FRAME_SIZE, MAGIC, VERSION};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, FILE_MAP_READ};

/// A consistent copy of one published frame.
#[derive(Default, Clone)]
pub struct Snapshot {
    pub frame: u32,
    pub origin: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub has_back: bool,
    pub sprites: Vec<Sprite>,
    /// Background drawing (pastes and quick-backdrop fills), in order.
    pub back_ops: Vec<BackOp>,
    /// 0x00RRGGBB, top-down.
    pub screen: Vec<u32>,
    pub back: Vec<u32>,
}

pub struct Reader {
    view: *const Frame,
    last_frame: u32,
}

impl Reader {
    /// Opens the mapping if the game is running; None otherwise.
    pub fn open() -> Option<Reader> {
        let name: Vec<u16> = hdshared::MAP_NAME.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let map: HANDLE = OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(name.as_ptr())).ok()?;
            let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, FRAME_SIZE);
            if view.Value.is_null() {
                return None;
            }
            Some(Reader { view: view.Value as *const Frame, last_frame: u32::MAX })
        }
    }

    /// Returns the latest frame if it is new and was read consistently.
    pub fn poll(&mut self) -> Option<Snapshot> {
        unsafe {
            let f = &*self.view;
            let h: &Header = &f.header;
            if h.magic != MAGIC || h.version != VERSION {
                return None;
            }
            let s1 = h.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                return None;
            }
            let frame = std::ptr::read_volatile(&h.frame);
            if frame == self.last_frame {
                return None;
            }
            let n_spr = (std::ptr::read_volatile(&h.n_sprites) as usize).min(f.sprites.len());
            let n_ops = (std::ptr::read_volatile(&h.n_back_ops) as usize).min(f.back_ops.len());
            let (w, hgt) = (std::ptr::read_volatile(&h.screen_w), std::ptr::read_volatile(&h.screen_h));
            let px = (w as usize * hgt as usize).min(f.screen.len());
            let has_back = std::ptr::read_volatile(&h.has_back) != 0;
            let snap = Snapshot {
                frame,
                origin: (std::ptr::read_volatile(&h.origin_x), std::ptr::read_volatile(&h.origin_y)),
                width: w,
                height: hgt,
                has_back,
                sprites: f.sprites[..n_spr].to_vec(),
                back_ops: f.back_ops[..n_ops].to_vec(),
                screen: f.screen[..px].to_vec(),
                back: if has_back { f.back[..px].to_vec() } else { Vec::new() },
            };
            if h.seq.load(Ordering::Acquire) != s1 {
                return None; // overwritten while copying; take the next one
            }
            self.last_frame = frame;
            Some(snap)
        }
    }
}
