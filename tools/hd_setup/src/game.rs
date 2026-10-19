//! Reading a Multimedia Fusion 2 (build 251) standalone exe: the runtime pack after the PE stub,
//! then the game data ("PAME") with its image bank. Same formats as extract_sprites.py.

use std::io::Read;

const PACK_MAGIC: &[u8; 8] = b"wwwwI\x87G\x12";
const IMAGE_BANK: u16 = 0x6666;
const LAST_CHUNK: u16 = 0x7F7F;
const FLAG_RLE: u8 = 0x01;
const FLAG_RLEW: u8 = 0x02;
const FLAG_RLET: u8 = 0x04;
const FLAG_ALPHA: u8 = 0x10;

pub struct PackFile {
    pub name: String,
    pub data: Vec<u8>,
}

pub struct Pack {
    pub offset: usize,
    pub files: Vec<PackFile>,
    pub game_offset: usize,
}

pub struct Image {
    pub handle: u32,
    pub width: usize,
    pub height: usize,
    pub hotspot: (i16, i16),
    pub action_point: (i16, i16),
    pub has_alpha: bool,
    pub rle: bool,
    pub key: [u8; 3],
    /// Straight RGBA; colour-keyed pixels (no alpha plane) are transparent.
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn file_name(&self) -> String {
        format!("{:05}.png", self.handle)
    }
}

fn u16_at(d: &[u8], o: usize) -> Result<u16, String> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(truncated)
}

fn u32_at(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(truncated)
}

fn truncated() -> String {
    "the file is truncated or isn't a Psycho Waluigi game exe".into()
}

fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).map_err(|e| format!("bad compressed data: {e}"))?;
    Ok(out)
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

/// The runtime pack starts where the PE image's sections end.
pub fn read_pack(exe: &[u8]) -> Result<Pack, String> {
    if exe.get(0..2) != Some(b"MZ") {
        return Err("not a Windows program".into());
    }
    let pe = u32_at(exe, 0x3C)? as usize;
    if exe.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("not a Windows program".into());
    }
    let nsec = u16_at(exe, pe + 6)? as usize;
    let sec = pe + 24 + u16_at(exe, pe + 20)? as usize;
    let mut offset = 0;
    for i in 0..nsec {
        offset = offset.max(u32_at(exe, sec + i * 40 + 20)? as usize + u32_at(exe, sec + i * 40 + 16)? as usize);
    }
    if exe.get(offset..offset + 8) != Some(&PACK_MAGIC[..]) {
        return Err("this exe has no Clickteam runtime pack (is it the original psychowaluigi.exe?)".into());
    }
    let hdr = u32_at(exe, offset + 8)? as usize;
    let count = u32_at(exe, offset + 28)? as usize;
    let mut pos = offset + hdr;
    let mut files = Vec::new();
    for _ in 0..count {
        let n = u16_at(exe, pos)? as usize;
        let name = String::from_utf8_lossy(exe.get(pos + 2..pos + 2 + n).ok_or_else(truncated)?).into_owned();
        pos += 2 + n + 4; // name, "bingo"
        let size = u32_at(exe, pos)? as usize;
        pos += 4;
        let data = inflate(exe.get(pos..pos + size).ok_or_else(truncated)?)?;
        pos += size;
        files.push(PackFile { name, data });
    }
    if !matches!(exe.get(pos..pos + 4), Some(b"PAME") | Some(b"PAMU")) {
        return Err("no game data after the runtime pack".into());
    }
    Ok(Pack { offset, files, game_offset: pos })
}

fn find_chunk(exe: &[u8], start: usize, wanted: u16) -> Result<&[u8], String> {
    let mut pos = start + 16;
    while pos + 8 <= exe.len() {
        let id = u16_at(exe, pos)?;
        let size = u32_at(exe, pos + 4)? as usize;
        pos += 8;
        if id == wanted {
            return exe.get(pos..pos + size).ok_or_else(truncated);
        }
        if id == LAST_CHUNK {
            break;
        }
        pos += size;
    }
    Err(format!("chunk {wanted:#x} not found in the game data"))
}

/// (handle, decompressed image record) for every image in the bank.
fn bank_records(exe: &[u8], game: usize) -> Result<Vec<(u32, Vec<u8>)>, String> {
    let bank = find_chunk(exe, game, IMAGE_BANK)?;
    let count = u32_at(bank, 0)? as usize;
    let mut pos = 4;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let handle = u32_at(bank, pos)?;
        let csz = u32_at(bank, pos + 8)? as usize;
        pos += 12;
        out.push((handle, inflate(bank.get(pos..pos + csz).ok_or_else(truncated)?)?));
        pos += csz;
    }
    Ok(out)
}

struct Header {
    size: usize,
    w: usize,
    h: usize,
    mode: u8,
    flags: u8,
    hot: (i16, i16),
    act: (i16, i16),
}

fn header(raw: &[u8]) -> Result<Header, String> {
    let i16_at = |o| u16_at(raw, o).map(|v| v as i16);
    Ok(Header {
        size: u32_at(raw, 8)? as usize,
        w: u16_at(raw, 12)? as usize,
        h: u16_at(raw, 14)? as usize,
        mode: *raw.get(16).ok_or_else(truncated)?,
        flags: *raw.get(17).ok_or_else(truncated)?,
        hot: (i16_at(20)?, i16_at(22)?),
        act: (i16_at(24)?, i16_at(26)?),
    })
}

/// MMF2 RLE: 0 ends; >128 is (n-128) literal pixels; otherwise the next pixel n times.
/// Returns the decoded bytes and how many input bytes were used.
fn unrle(data: &[u8], point: usize, expected: usize) -> Result<(Vec<u8>, usize), String> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < data.len() {
        let n = data[i] as usize;
        i += 1;
        if n == 0 {
            break;
        }
        if n > 128 {
            let n = (n - 128) * point;
            out.extend_from_slice(data.get(i..i + n).ok_or_else(truncated)?);
            i += n;
        } else {
            let px = data.get(i..i + point).ok_or_else(truncated)?;
            for _ in 0..n {
                out.extend_from_slice(px);
            }
            i += point;
        }
    }
    if out.len() != expected {
        return Err(format!("RLE produced {} bytes, expected {expected}", out.len()));
    }
    Ok((out, i))
}

/// Bytes of an RLE stream needed for `pixels` pixels (what the proxy hashes in AddImage).
fn rle_length(data: &[u8], pixels: usize) -> usize {
    let (mut i, mut done) = (0, 0);
    while done < pixels && i < data.len() {
        let n = data[i] as usize;
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

fn decode(handle: u32, raw: &[u8]) -> Result<Image, String> {
    let hd = header(raw)?;
    if hd.mode != 6 {
        return Err(format!("image {handle}: graphic mode {} not supported", hd.mode));
    }
    if hd.flags & (FLAG_RLE | FLAG_RLET) != 0 {
        return Err(format!("image {handle}: flags {:#x} not supported", hd.flags));
    }
    let key = [raw[28], raw[29], raw[30]];
    let body = raw.get(32..32 + hd.size).ok_or_else(truncated)?;
    let (w, h) = (hd.w, hd.h);
    let row = w * 2;
    let (color, rest): (std::borrow::Cow<[u8]>, &[u8]) = if hd.flags & FLAG_RLEW != 0 {
        let (c, used) = unrle(body, 2, row * h)?;
        (c.into(), &body[used..])
    } else {
        let c = body.get(..row * h).ok_or_else(truncated)?;
        (c.into(), &body[row * h..])
    };
    let arow = w + (4 - w % 4) % 4;
    let alpha = if hd.flags & FLAG_ALPHA != 0 {
        Some(rest.get(..arow * h).ok_or(format!("image {handle}: alpha plane truncated"))?)
    } else {
        None
    };
    let key555 = ((key[0] as u16 >> 3) << 10) | ((key[1] as u16 >> 3) << 5) | (key[2] as u16 >> 3);
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let v = u16::from_le_bytes([color[y * row + 2 * x], color[y * row + 2 * x + 1]]);
            let ex = |c: u16| ((c << 3) | (c >> 2)) as u8;
            let a = match alpha {
                Some(al) => al[y * arow + x],
                None if v == key555 => 0,
                None => 255,
            };
            rgba[(y * w + x) * 4..][..4].copy_from_slice(&[ex((v >> 10) & 31), ex((v >> 5) & 31), ex(v & 31), a]);
        }
    }
    Ok(Image {
        handle,
        width: w,
        height: h,
        hotspot: hd.hot,
        action_point: hd.act,
        has_alpha: alpha.is_some(),
        rle: hd.flags & FLAG_RLEW != 0,
        key,
        rgba,
    })
}

/// Every image the bank holds in a supported format, in bank order.
pub fn read_image_bank(exe: &[u8], game: usize) -> Result<Vec<Image>, String> {
    let recs = bank_records(exe, game)?;
    let mut out = Vec::with_capacity(recs.len());
    let mut skipped = 0;
    for (handle, raw) in &recs {
        match decode(*handle, raw) {
            Ok(img) if img.width > 0 && img.height > 0 => out.push(img),
            Ok(_) => skipped += 1,
            Err(e) => {
                println!("  skipped {e}");
                skipped += 1;
            }
        }
    }
    println!("{} images ({skipped} skipped)", out.len());
    if out.is_empty() {
        return Err("the game's image bank is empty".into());
    }
    Ok(out)
}

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01B3))
}

/// "<w h xhot yhot xact yact fnv1a64(colour plane as stored)>\t<bank handle>" per distinct key,
/// lowest handle per key, sorted by handle (tools/build_image_keys.py).
pub fn image_keys(exe: &[u8], game: usize) -> Result<String, String> {
    let mut keys: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (handle, raw) in bank_records(exe, game)? {
        let hd = header(&raw)?;
        let body = raw.get(32..32 + hd.size).ok_or_else(truncated)?;
        let n = if hd.flags & FLAG_RLEW != 0 { rle_length(body, hd.w * hd.h) } else { hd.w * hd.h * 2 };
        let key = format!("{} {} {} {} {} {} {:016x}", hd.w, hd.h, hd.hot.0, hd.hot.1, hd.act.0, hd.act.1,
                          fnv1a64(&body[..n.min(body.len())]));
        let e = keys.entry(key).or_insert(handle);
        *e = (*e).min(handle);
    }
    let mut rows: Vec<(String, u32)> = keys.into_iter().collect();
    rows.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    Ok(rows.iter().map(|(k, h)| format!("{k}\t{h}\n")).collect())
}
