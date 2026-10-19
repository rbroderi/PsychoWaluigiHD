//! Builds the HD version of Psycho Waluigi from the user's own copy of the game. The installer
//! (installer/psycho_waluigi_hd.iss) runs it from its temp folder, with cleanedge.exe beside it.
//!
//! Usage: hd_setup --game <psychowaluigi.exe> --dest <install dir>
//!        hd_setup --check --game <psychowaluigi.exe>   (only checks the exe is a supported copy)
//!
//! Into <dest> it writes:
//!   psychowaluigi.exe                 copy of the game (the runtime reads its data from it)
//!   hd_runtime/stdrt.exe, mmfs2_orig.dll, *.mfx, *.sft, CCTrans.dll   the game's own runtime,
//!                                     unpacked from the exe (mmfs2.dll there is the proxy)
//!   hd_runtime/image_keys.tsv         image fingerprints for the proxy (tools/build_image_keys.py)
//!   hd_runtime/image_masks.bin        opacity masks for the proxy (tools/build_image_masks.py)
//!   sprites/                          the image bank as PNGs + index.json (extract_sprites.py)
//!   sprites_xbrz9x_direct/            xBRZ 6x + Lanczos to 9x (upscale_sprites.py 9 --direct)
//!   sprites_cleanedge9x/              cleanEdge 9x, by running the cleanedge.exe next to hd_setup.exe
//! The outputs match what those scripts produce for the same exe, pixel for pixel.
//!
//! Progress lines on stdout read "@<percent>|<text>"; the installer shows them. Errors go to
//! stderr as "error: <message>" with exit code 1.

mod game;
mod lanczos;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

/// The runtime the proxy and viewer were made for (MMF2 build 251), as packed in the game exe.
const PACK_OFFSET: usize = 94208;
const KNOWN_RUNTIME: [(&str, u32); 2] = [("stdrt.exe", 0x986d_1246), ("mmfs2.dll", 0x3c0c_754f)];
const SCALE: usize = 9;
const XBRZ_MAX: usize = 6;
/// Low-res pixels of repeated edge around each image while scaling (as upscale_sprites.py).
const PAD: usize = 2;

struct Progress {
    /// (start, width) of the current stage in overall percent.
    span: (f32, f32),
    text: String,
}

impl Progress {
    fn stage(&mut self, start: f32, width: f32, text: &str) {
        self.span = (start, width);
        self.text = text.to_string();
        println!("@{start:.0}|{text}");
    }

    fn update(&self, done: usize, total: usize) {
        if done % 50 != 0 && done != total {
            return;
        }
        let pct = self.span.0 + self.span.1 * done as f32 / total.max(1) as f32;
        println!("@{pct:.0}|{} ({done} of {total})", self.text);
    }
}

fn main() {
    let mut game = None;
    let mut dest = None;
    let mut check = false;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--check" => check = true,
            "--game" => game = it.next().map(PathBuf::from),
            "--dest" => dest = it.next().map(PathBuf::from),
            _ => {}
        }
    }
    let result = match (game, dest) {
        (Some(game), _) if check => std::panic::catch_unwind(|| read_game(&game).map(|_| ())),
        (Some(game), Some(dest)) => std::panic::catch_unwind(|| {
            run(&game, &dest, &mut Progress { span: (0.0, 0.0), text: String::new() })
        }),
        _ => {
            eprintln!("usage: hd_setup --game <psychowaluigi.exe> --dest <install dir>
       hd_setup --check --game <psychowaluigi.exe>");
            std::process::exit(2);
        }
    };
    match result {
        Ok(Ok(())) => println!("@100|done"),
        Ok(Err(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("error: internal error (see the message above)");
            std::process::exit(1);
        }
    }
}

/// Reads the exe and checks it carries the runtime the HD version was made for.
fn read_game(path: &Path) -> Result<(Vec<u8>, game::Pack, Vec<game::Image>), String> {
    let exe = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let pack = game::read_pack(&exe)?;
    if pack.offset != PACK_OFFSET {
        return Err(format!("unexpected runtime pack position {} (this installer supports the release built with MMF2 build 251)", pack.offset));
    }
    for (name, crc) in KNOWN_RUNTIME {
        let f = pack.files.iter().find(|f| f.name.eq_ignore_ascii_case(name)).ok_or(format!("the game has no {name}"))?;
        if game::crc32(&f.data) != crc {
            return Err(format!("{name} in this game is a different version than the one the HD version was made for (MMF2 build 251)"));
        }
    }
    let images = game::read_image_bank(&exe, pack.game_offset)?;
    Ok((exe, pack, images))
}

fn run(game_path: &Path, dest: &Path, p: &mut Progress) -> Result<(), String> {
    p.stage(0.0, 1.0, "Reading the game");
    let (exe, pack, images) = read_game(game_path)?;

    let runtime = dest.join("hd_runtime");
    mkdir(&runtime)?;
    p.stage(1.0, 2.0, "Copying the game");
    let game_copy = dest.join("psychowaluigi.exe");
    if !same_file(game_path, &game_copy) {
        write(&game_copy, &exe)?;
    }
    for f in &pack.files {
        // The proxy is installed as mmfs2.dll; it loads the real one as mmfs2_orig.dll.
        let name = if f.name.eq_ignore_ascii_case("mmfs2.dll") { "mmfs2_orig.dll" } else { f.name.as_str() };
        write(&runtime.join(name), &f.data)?;
    }

    p.stage(3.0, 4.0, "Extracting the original sprites");
    let sprites = dest.join("sprites");
    mkdir(&sprites)?;
    let done = AtomicUsize::new(0);
    images.par_iter().try_for_each(|img| {
        save_png(&sprites.join(img.file_name()), img.width, img.height, &img.rgba)?;
        p.update(done.fetch_add(1, Ordering::Relaxed) + 1, images.len());
        Ok::<_, String>(())
    })?;
    write(&sprites.join("index.json"), index_json(&images, 1).as_bytes())?;

    p.stage(7.0, 1.0, "Building image keys and masks");
    write(&runtime.join("image_keys.tsv"), game::image_keys(&exe, pack.game_offset)?.as_bytes())?;
    write(&runtime.join("image_masks.bin"), &image_masks(&images))?;

    p.stage(8.0, 47.0, "Upscaling textures (xBRZ)");
    let xbrz_dir = dest.join("sprites_xbrz9x_direct");
    mkdir(&xbrz_dir)?;
    let done = AtomicUsize::new(0);
    images.par_iter().try_for_each(|img| {
        let (w, h, rgba) = xbrz_scale(img);
        save_png(&xbrz_dir.join(img.file_name()), w, h, &rgba)?;
        p.update(done.fetch_add(1, Ordering::Relaxed) + 1, images.len());
        Ok::<_, String>(())
    })?;
    write(&xbrz_dir.join("index.json"), index_json(&images, SCALE).as_bytes())?;

    p.stage(55.0, 45.0, "Upscaling textures (cleanEdge)");
    let tool = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("cleanedge.exe");
    run_cleanedge(&tool, &sprites, &dest.join("sprites_cleanedge9x"), images.len(), p)?;
    Ok(())
}

/// xBRZ on the transparent sprite with a repeated-edge border, 6x, then Lanczos to 9x, crop.
fn xbrz_scale(img: &game::Image) -> (usize, usize, Vec<u8>) {
    let (w, h) = (img.width, img.height);
    let (pw, ph) = (w + 2 * PAD, h + 2 * PAD);
    let mut padded = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        let sy = y.saturating_sub(PAD).min(h - 1);
        for x in 0..pw {
            let sx = x.saturating_sub(PAD).min(w - 1);
            let s = (sy * w + sx) * 4;
            padded[(y * pw + x) * 4..][..4].copy_from_slice(&img.rgba[s..s + 4]);
        }
    }
    let big = xbrz::scale_rgba(&padded, pw, ph, XBRZ_MAX);
    let scaled = lanczos::resize_rgba(&big, pw * XBRZ_MAX, ph * XBRZ_MAX, pw * SCALE, ph * SCALE);
    let (ow, oh) = (w * SCALE, h * SCALE);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        let s = ((y + PAD * SCALE) * pw * SCALE + PAD * SCALE) * 4;
        out[y * ow * 4..(y + 1) * ow * 4].copy_from_slice(&scaled[s..s + ow * 4]);
    }
    (ow, oh, out)
}

fn run_cleanedge(tool: &Path, src: &Path, out: &Path, total: usize, p: &Progress) -> Result<(), String> {
    let mut child = Command::new(tool)
        .arg(src)
        .arg(out)
        .arg(SCALE.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't start {}: {e}", tool.display()))?;
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some((n, _)) = line.split_once('/') {
            if let Ok(n) = n.trim().parse::<usize>() {
                p.update(n, total);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cleanEdge upscaling failed ({status})"));
    }
    Ok(())
}

/// The sprite index as extract_sprites.py writes it; scaled as upscale_sprites.py does for HD sets.
fn index_json(images: &[game::Image], scale: usize) -> String {
    let s = scale as i64;
    let mut map = serde_json::Map::new();
    for img in images {
        let mut m = serde_json::json!({
            "width": img.width as i64 * s,
            "height": img.height as i64 * s,
            "hotspot": [img.hotspot.0 as i64 * s, img.hotspot.1 as i64 * s],
            "action_point": [img.action_point.0 as i64 * s, img.action_point.1 as i64 * s],
            "alpha": img.has_alpha,
            "rle": img.rle,
            "transparent_color": img.key,
        });
        if scale != 1 {
            m["scale"] = scale.into();
        }
        map.insert(img.handle.to_string(), m);
    }
    serde_json::to_string_pretty(&map).unwrap()
}

/// b"MASK", count, then per image: bank, width, height, rows of ceil(width/8) bytes (bit x%8).
fn image_masks(images: &[game::Image]) -> Vec<u8> {
    let mut sorted: Vec<&game::Image> = images.iter().collect();
    sorted.sort_by_key(|i| i.handle);
    let mut out = b"MASK".to_vec();
    out.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    for img in sorted {
        out.extend_from_slice(&img.handle.to_le_bytes());
        out.extend_from_slice(&(img.width as u16).to_le_bytes());
        out.extend_from_slice(&(img.height as u16).to_le_bytes());
        let row = img.width.div_ceil(8);
        for y in 0..img.height {
            let mut bits = vec![0u8; row];
            for x in 0..img.width {
                if img.rgba[(y * img.width + x) * 4 + 3] >= 128 {
                    bits[x / 8] |= 1 << (x % 8);
                }
            }
            out.extend_from_slice(&bits);
        }
    }
    out
}

fn save_png(path: &Path, w: usize, h: usize, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| format!("{}: {e}", path.display()))?;
    wr.write_image_data(rgba).map_err(|e| format!("{}: {e}", path.display()))
}

fn mkdir(p: &Path) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("can't create {}: {e}", p.display()))
}

fn write(p: &Path, data: &[u8]) -> Result<(), String> {
    std::fs::write(p, data).map_err(|e| format!("can't write {}: {e}", p.display()))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
