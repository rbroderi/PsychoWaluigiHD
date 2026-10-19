//! Opacity masks of the bank images (hd_runtime/image_masks.bin, written by
//! tools/build_image_masks.py), so background ops can be pruned by what they really cover.

use std::collections::HashMap;
use std::path::Path;

pub struct Mask {
    pub width: i32,
    pub height: i32,
    row_bytes: usize,
    offset: usize,
}

pub struct Masks {
    by_bank: HashMap<u32, Mask>,
    bits: Vec<u8>,
}

impl Masks {
    pub fn load(path: &Path) -> Option<Masks> {
        let data = std::fs::read(path).ok()?;
        if data.get(..4)? != b"MASK" {
            return None;
        }
        let count = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?) as usize;
        let mut by_bank = HashMap::with_capacity(count);
        let mut pos = 8;
        for _ in 0..count {
            let bank = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?);
            let width = u16::from_le_bytes(data.get(pos + 4..pos + 6)?.try_into().ok()?) as i32;
            let height = u16::from_le_bytes(data.get(pos + 6..pos + 8)?.try_into().ok()?) as i32;
            let row_bytes = (width as usize + 7) / 8;
            pos += 8;
            by_bank.insert(bank, Mask { width, height, row_bytes, offset: pos });
            pos += row_bytes * height as usize;
        }
        if pos > data.len() {
            return None;
        }
        Some(Masks { by_bank, bits: data })
    }

    pub fn get(&self, bank: u32) -> Option<&Mask> {
        self.by_bank.get(&bank)
    }

    /// Whether pixel (x, y) of `mask` is opaque; outside the image counts as transparent.
    pub fn opaque(&self, mask: &Mask, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= mask.width || y >= mask.height {
            return false;
        }
        let byte = self.bits[mask.offset + y as usize * mask.row_bytes + x as usize / 8];
        byte & (1 << (x % 8)) != 0
    }
}
