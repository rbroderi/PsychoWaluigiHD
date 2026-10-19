//! Frame protocol between the 32-bit mmfs2 proxy (writer) and the 64-bit HD viewer (reader).
//!
//! The proxy owns a named file mapping holding one [`Frame`]. It is guarded by a sequence lock:
//! the writer bumps `seq` to an odd value, writes, then bumps it to the next even value. A reader
//! copies what it needs and accepts the copy only if `seq` was even and unchanged throughout.
//! Every field is fixed-size so the layout is identical in 32- and 64-bit builds.
#![no_std]

use core::sync::atomic::AtomicU32;

/// Name of the file mapping (UTF-16 at use sites).
pub const MAP_NAME: &str = "Local\\PsychoWaluigiHD";
pub const MAGIC: u32 = 0x4844_5057;
pub const VERSION: u32 = 2;

pub const MAX_SPRITES: usize = 4096;
pub const MAX_BACK_OPS: usize = 8192;
pub const MAX_SCREEN_W: usize = 640;
pub const MAX_SCREEN_H: usize = 480;

pub const KIND_IMAGE: u32 = 0;
pub const KIND_OWNERDRAW: u32 = 1;
pub const NO_BANK: u32 = u32::MAX;

pub const SPRITE_HIDDEN: u32 = 0x80;

/// Ink effects (low 16 bits of `effect`), from the SDK's BlitOp enum.
pub const BOP_COPY: u32 = 0;
pub const BOP_BLEND: u32 = 1;
pub const BOP_ADD: u32 = 9;
pub const BOP_SUB: u32 = 11;
pub const EFFECT_MASK: u32 = 0xFFFF;

#[repr(C)]
pub struct Header {
    pub magic: u32,
    pub version: u32,
    pub seq: AtomicU32,
    /// SpriteDraw call counter in the game.
    pub frame: u32,
    /// Top-left of the visible area in logical (playfield) coordinates.
    pub origin_x: i32,
    pub origin_y: i32,
    pub screen_w: u32,
    pub screen_h: u32,
    /// 1 if `back` holds the background-only surface for this frame.
    pub has_back: u32,
    pub n_sprites: u32,
    pub n_back_ops: u32,
    /// Incremented whenever the background op list is cleared (level change).
    pub back_epoch: u32,
    pub reserved: [u32; 4],
}

/// One sprite, in draw order (back to front).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Sprite {
    pub kind: u32,
    /// Original image-bank handle, or NO_BANK.
    pub bank: u32,
    /// Hot spot position, logical coordinates.
    pub x: i32,
    pub y: i32,
    /// Bounding box as drawn (after scale/rotation), logical coordinates.
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub scale_x: f32,
    pub scale_y: f32,
    /// Degrees, counter-clockwise.
    pub angle: i32,
    pub effect: u32,
    pub param: u32,
    pub layer: u32,
    pub z: i32,
    /// Raw sprite flags (SF_*).
    pub flags: u32,
}

pub const OP_PASTE: u32 = 0;
pub const OP_TILE: u32 = 1;
pub const OP_GRADIENT: u32 = 2;
pub const OP_FLAT: u32 = 3;
/// Part of an image blitted into the background (how quick backdrops tile their pattern).
pub const OP_BLIT: u32 = 4;

/// Something drawn into the level background, in drawing order. Colours are COLORREF
/// (0x00BBGGRR); coordinates are logical.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct BackOp {
    pub kind: u32,
    /// OP_PASTE / OP_BLIT: the image; OP_TILE: the tile image. NO_BANK otherwise.
    pub bank: u32,
    /// OP_PASTE: top-left corner (pastes ignore the hot spot). OP_BLIT: destination.
    /// Fills: the rectangle's left/top.
    pub x: i32,
    pub y: i32,
    /// Fills: rectangle size. OP_BLIT: size of the copied part. OP_PASTE: the image's size.
    pub w: i32,
    pub h: i32,
    /// OP_TILE / OP_GRADIENT: pattern origin. OP_BLIT: top-left of the copied part in the image.
    pub org_x: i32,
    pub org_y: i32,
    /// OP_FLAT: colour; OP_GRADIENT: from/to colours.
    pub color1: u32,
    pub color2: u32,
    /// OP_GRADIENT: 1 if vertical (top to bottom).
    pub vertical: u32,
    pub effect: u32,
    pub param: u32,
}

pub const SCREEN_PIXELS: usize = MAX_SCREEN_W * MAX_SCREEN_H;

/// Whole shared block. Pixels are 0x00RRGGBB, rows top-down, `screen_w` per row.
#[repr(C)]
pub struct Frame {
    pub header: Header,
    pub sprites: [Sprite; MAX_SPRITES],
    pub back_ops: [BackOp; MAX_BACK_OPS],
    /// What the game drew this frame (backdrops + sprites).
    pub screen: [u32; SCREEN_PIXELS],
    /// Background without sprites, when `has_back` is set.
    pub back: [u32; SCREEN_PIXELS],
}

pub const FRAME_SIZE: usize = core::mem::size_of::<Frame>();
