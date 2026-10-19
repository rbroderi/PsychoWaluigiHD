//! Upscales the extracted sprites with torcado's cleanEdge pixel-art scaling algorithm
//! (third_party/torcado_gist/cleanEdge-shadertoy.glsl, MIT licence, (c) 2022 torcado), ported
//! from the shader to run per output pixel on the CPU. Settings are the shader's defaults:
//! SLOPE and CLEANUP on, highest-priority colour white, similarity threshold 0, line width 1.
//!
//! Each image gets a border of repeated edge pixels while scaling (then cut off), like the
//! xBRZ set, so images the game puts side by side don't show seams.
//!
//! Usage: cleanedge <sprites_dir> <out_dir> [scale=9]
//! Writes <out_dir>/<bank>.png for every image in <sprites_dir>/index.json, plus a scaled index.json.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

type Col = [f32; 4];

const HIGHEST: [f32; 3] = [1.0, 1.0, 1.0];
const SIMILAR_THRESHOLD: f32 = 0.0;
const LINE_WIDTH: f32 = 1.0;
/// Edge pixels repeated around each image while scaling (the shader looks 2 pixels out).
const PAD: i32 = 2;

fn distance4(a: &Col, b: &Col) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2) + (a[3] - b[3]).powi(2)).sqrt()
}

fn distance_rgb(a: &Col, b: &[f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn similar(a: &Col, b: &Col) -> bool {
    (a[3] == 0.0 && b[3] == 0.0) || distance4(a, b) <= SIMILAR_THRESHOLD
}

fn similar3(a: &Col, b: &Col, c: &Col) -> bool {
    similar(a, b) && similar(b, c)
}

fn similar4(a: &Col, b: &Col, c: &Col, d: &Col) -> bool {
    similar(a, b) && similar(b, c) && similar(c, d)
}

fn higher(this: &Col, other: &Col) -> bool {
    if similar(this, other) {
        return false;
    }
    if this[3] == other[3] {
        distance_rgb(this, &HIGHEST) < distance_rgb(other, &HIGHEST)
    } else {
        this[3] > other[3]
    }
}

fn cd(a: &Col, b: &Col) -> f32 {
    distance4(a, b)
}

type V2 = [f32; 2];

fn add(a: V2, b: V2) -> V2 {
    [a[0] + b[0], a[1] + b[1]]
}

fn mul(a: V2, b: V2) -> V2 {
    [a[0] * b[0], a[1] * b[1]]
}

fn neg(a: V2) -> V2 {
    [-a[0], -a[1]]
}

fn dist_to_line(test: V2, pt1: V2, pt2: V2, dir: V2) -> f32 {
    let line = [pt2[0] - pt1[0], pt2[1] - pt1[1]];
    let perp = [line[1], -line[0]];
    let to_pt1 = [pt1[0] - test[0], pt1[1] - test[1]];
    let len = (perp[0] * perp[0] + perp[1] * perp[1]).sqrt();
    let sign = if perp[0] * dir[0] + perp[1] * dir[1] > 0.0 { 1.0 } else { -1.0 };
    sign * ((perp[0] / len) * to_pt1[0] + (perp[1] / len) * to_pt1[1])
}

/// The 21-pixel neighbourhood of a slice, named as in the shader (relative to its quadrant).
struct N {
    ub: Col, u: Col, uf: Col, uff: Col,
    b: Col, c: Col, f: Col, ff: Col,
    db: Col, d: Col, df: Col, dff: Col,
    ddb: Col, dd: Col, ddf: Col,
}

/// Port of sliceDist: the colour of the slice covering `point`, or None.
fn slice_dist(point: V2, main_dir: V2, pd: V2, n: &N) -> Option<Col> {
    let (min_w, max_w) = (0.45f32, 1.142f32); // SLOPE
    let lw = min_w.max(max_w.min(LINE_WIDTH));
    let point = [main_dir[0] * (point[0] - 0.5) + 0.5, main_dir[1] * (point[1] - 0.5) + 0.5];
    let N { ub, u, uf, uff, b, c, f, ff, db, d, df, dff, ddb, dd, ddf } = n;

    // edge detection
    let dist_against = 4.0 * cd(f, d) + cd(uf, c) + cd(c, db) + cd(ff, df) + cd(df, dd);
    let dist_towards = 4.0 * cd(c, df) + cd(u, f) + cd(f, dff) + cd(b, d) + cd(d, ddf);
    let mut should_slice = dist_against < dist_towards || (dist_against < dist_towards + 0.001 && !higher(c, f));
    if similar4(f, d, b, u) && similar4(uf, df, db, ub) && !similar(c, f) {
        should_slice = false; // checkerboard
    }
    if !should_slice {
        return None;
    }
    let mut flip = false;
    let center = [0.5f32, 0.5];
    let at = |v: V2| add(center, mul(v, pd));

    if similar3(f, d, db) && !similar3(f, d, b) && !similar(uf, db) {
        // lower shallow 2:1 slant
        if !(similar(c, df) && higher(c, f)) {
            if higher(c, f) {
                flip = true;
            }
            if similar(u, f) && !similar(c, df) && !higher(c, u) {
                flip = true;
            }
        }
        let mut dist = if flip {
            lw - dist_to_line(point, at([1.5, -1.0]), at([-0.5, 0.0]), neg(pd))
        } else {
            dist_to_line(point, at([1.5, 0.0]), at([-0.5, 1.0]), pd)
        };
        // CLEANUP
        if !flip && similar(c, uf) && !(similar3(c, uf, uff) && !similar3(c, uf, ff) && !similar(d, uff)) {
            let d2 = dist_to_line(point, at([2.0, -1.0]), at([0.0, 1.0]), pd);
            dist = dist.min(d2);
        }
        dist -= lw / 2.0;
        return if dist <= 0.0 { Some(if cd(c, f) <= cd(c, d) { *f } else { *d }) } else { None };
    } else if similar3(uf, f, d) && !similar3(u, f, d) && !similar(uf, db) {
        // forward steep 2:1 slant
        if !(similar(c, df) && higher(c, d)) {
            if higher(c, d) {
                flip = true;
            }
            if similar(b, d) && !similar(c, df) && !higher(c, d) {
                flip = true;
            }
        }
        let mut dist = if flip {
            lw - dist_to_line(point, at([0.0, -0.5]), at([-1.0, 1.5]), neg(pd))
        } else {
            dist_to_line(point, at([1.0, -0.5]), at([0.0, 1.5]), pd)
        };
        if !flip && similar(c, db) && !(similar3(c, db, ddb) && !similar3(c, db, dd) && !similar(f, ddb)) {
            let d2 = dist_to_line(point, at([1.0, 0.0]), at([-1.0, 2.0]), pd);
            dist = dist.min(d2);
        }
        dist -= lw / 2.0;
        return if dist <= 0.0 { Some(if cd(c, f) <= cd(c, d) { *f } else { *d }) } else { None };
    } else if similar(f, d) {
        // 45 degree diagonal
        if similar(c, df) && higher(c, f) {
            if !similar(c, dd) && !similar(c, ff) {
                flip = true;
            }
        } else {
            if higher(c, f) {
                flip = true;
            }
            if !similar(c, b) && similar4(b, f, d, u) {
                flip = true;
            }
        }
        if ((similar(f, db) && similar3(u, f, df)) || (similar(uf, d) && similar3(b, d, df))) && !similar(c, df) {
            flip = true;
        }
        let mut dist = if flip {
            lw - dist_to_line(point, at([1.0, -1.0]), at([-1.0, 1.0]), neg(pd))
        } else {
            dist_to_line(point, at([1.0, 0.0]), at([0.0, 1.0]), pd)
        };
        // SLOPE + CLEANUP
        if !flip && similar3(c, uf, uff) && !similar3(c, uf, ff) && !similar(d, uff) {
            let d2 = dist_to_line(point, at([1.5, 0.0]), at([-0.5, 1.0]), pd);
            dist = dist.max(d2);
        }
        if !flip && similar3(ddb, db, c) && !similar3(dd, db, c) && !similar(ddb, f) {
            let d2 = dist_to_line(point, at([1.0, -0.5]), at([0.0, 1.5]), pd);
            dist = dist.max(d2);
        }
        dist -= lw / 2.0;
        return if dist <= 0.0 { Some(if cd(c, f) <= cd(c, d) { *f } else { *d }) } else { None };
    } else if similar3(ff, df, d) && !similar3(ff, df, c) && !similar(uff, d) {
        // far corner of shallow slant
        if !(similar(f, dff) && higher(f, ff)) {
            if higher(f, ff) {
                flip = true;
            }
            if similar(uf, ff) && !similar(f, dff) && !higher(f, uf) {
                flip = true;
            }
        }
        let mut dist = if flip {
            lw - dist_to_line(point, at([2.5, -1.0]), at([0.5, 0.0]), neg(pd))
        } else {
            dist_to_line(point, at([2.5, 0.0]), at([0.5, 1.0]), pd)
        };
        dist -= lw / 2.0;
        return if dist <= 0.0 { Some(if cd(f, ff) <= cd(f, df) { *ff } else { *df }) } else { None };
    } else if similar3(f, df, dd) && !similar3(c, df, dd) && !similar(f, ddb) {
        // far corner of steep slant
        if !(similar(d, ddf) && higher(d, dd)) {
            if higher(d, dd) {
                flip = true;
            }
            if similar(db, dd) && !similar(d, ddf) && !higher(d, dd) {
                flip = true;
            }
        }
        let mut dist = if flip {
            lw - dist_to_line(point, at([0.0, 0.5]), at([-1.0, 2.5]), neg(pd))
        } else {
            dist_to_line(point, at([1.0, 0.5]), at([0.0, 2.5]), pd)
        };
        dist -= lw / 2.0;
        return if dist <= 0.0 { Some(if cd(d, df) <= cd(d, dd) { *df } else { *dd }) } else { None };
    }
    None
}

struct Img {
    w: i32,
    h: i32,
    px: Vec<Col>,
}

impl Img {
    /// Pixel (x, y), clamped to the image (the padded image repeats its edges).
    fn get(&self, x: i32, y: i32) -> Col {
        self.px[(y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize]
    }
}

/// Port of mainImage: the colour of output pixel (ox, oy) at `scale`.
fn sample(img: &Img, ox: i32, oy: i32, scale: f32) -> Col {
    let p = [(ox as f32 + 0.5) / scale, (oy as f32 + 0.5) / scale];
    let local = [p[0].fract(), p[1].fract()];
    let (cx, cy) = (p[0].floor() as i32, p[1].floor() as i32);
    // GLSL round(): halves round up here.
    let pd = [if local[0] >= 0.5 { 1.0 } else { -1.0 }, if local[1] >= 0.5 { 1.0 } else { -1.0 }];
    let (sx, sy) = (pd[0] as i32, pd[1] as i32);
    let g = |dx: i32, dy: i32| img.get(cx + dx * sx, cy + dy * sy);

    let (uub, uu, uuf) = (g(-1, -2), g(0, -2), g(1, -2));
    let (ubb, ub, u, uf, uff) = (g(-2, -1), g(-1, -1), g(0, -1), g(1, -1), g(2, -1));
    let (bb, b, c, f, ff) = (g(-2, 0), g(-1, 0), g(0, 0), g(1, 0), g(2, 0));
    let (dbb, db, d, df, dff) = (g(-2, 1), g(-1, 1), g(0, 1), g(1, 1), g(2, 1));
    let (ddb, dd, ddf) = (g(-1, 2), g(0, 2), g(1, 2));

    let mut col = c;
    let c_n = N { ub, u, uf, uff, b, c, f, ff, db, d, df, dff, ddb, dd, ddf };
    let b_n = N { ub: uf, u, uf: ub, uff: ubb, b: f, c, f: b, ff: bb, db: df, d, df: db, dff: dbb, ddb: ddf, dd, ddf: ddb };
    let u_n = N { ub: db, u: d, uf: df, uff: dff, b, c, f, ff, db: ub, d: u, df: uf, dff: uff, ddb: uub, dd: uu, ddf: uuf };
    if let Some(x) = slice_dist(local, [1.0, 1.0], pd, &c_n) {
        col = x;
    }
    if let Some(x) = slice_dist(local, [-1.0, 1.0], pd, &b_n) {
        col = x;
    }
    if let Some(x) = slice_dist(local, [1.0, -1.0], pd, &u_n) {
        col = x;
    }
    col
}

fn load_png(path: &Path) -> Option<Img> {
    let mut dec = png::Decoder::new(File::open(path).ok()?);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    let (w, h) = (info.width as i32, info.height as i32);
    // Padded by PAD repeated edge pixels on every side.
    let (pw, ph) = (w + 2 * PAD, h + 2 * PAD);
    let mut px = Vec::with_capacity((pw * ph) as usize);
    for y in 0..ph {
        for x in 0..pw {
            let (sx, sy) = ((x - PAD).clamp(0, w - 1), (y - PAD).clamp(0, h - 1));
            let i = ((sy * w + sx) * 4) as usize;
            let a = buf[i + 3] as f32 / 255.0;
            // Transparent pixels all compare equal (the shader's similar() checks alpha 0).
            px.push(if buf[i + 3] == 0 {
                [0.0, 0.0, 0.0, 0.0]
            } else {
                [buf[i] as f32 / 255.0, buf[i + 1] as f32 / 255.0, buf[i + 2] as f32 / 255.0, a]
            });
        }
    }
    Some(Img { w: pw, h: ph, px })
}

fn scale_image(src: &Path, dst: &Path, scale: i32) -> Result<(), String> {
    let img = load_png(src).ok_or_else(|| format!("cannot read {}", src.display()))?;
    let (w, h) = (img.w - 2 * PAD, img.h - 2 * PAD);
    let (ow, oh) = (w * scale, h * scale);
    let mut out = vec![0u8; (ow * oh * 4) as usize];
    for y in 0..oh {
        for x in 0..ow {
            let c = sample(&img, x + PAD * scale, y + PAD * scale, scale as f32);
            let i = ((y * ow + x) * 4) as usize;
            for k in 0..4 {
                out[i + k] = (c[k].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    let file = File::create(dst).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), ow as u32, oh as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(&out).map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: cleanedge <sprites_dir> <out_dir> [scale=9]");
        std::process::exit(2);
    }
    let (src, out) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let scale: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(9);
    std::fs::create_dir_all(&out).expect("output dir");
    let index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(src.join("index.json")).expect("index.json")).expect("index json");
    let handles: Vec<u32> = index.as_object().unwrap().keys().filter_map(|k| k.parse().ok()).collect();
    let done = AtomicUsize::new(0);
    let failed: Vec<String> = handles.par_iter().filter_map(|h| {
        let name = format!("{h:05}.png");
        let r = scale_image(&src.join(&name), &out.join(&name), scale).err();
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 500 == 0 {
            println!("{n}/{}", handles.len());
        }
        r
    }).collect();

    // Scaled index, as upscale_sprites.py writes it.
    let mut scaled = serde_json::Map::new();
    for (k, v) in index.as_object().unwrap() {
        let mut m = v.as_object().unwrap().clone();
        for key in ["width", "height"] {
            if let Some(x) = m.get(key).and_then(|x| x.as_i64()) {
                m.insert(key.into(), (x * scale as i64).into());
            }
        }
        for key in ["hotspot", "action_point"] {
            if let Some(a) = m.get(key).and_then(|x| x.as_array()) {
                let s: Vec<serde_json::Value> = a.iter().map(|x| (x.as_i64().unwrap_or(0) * scale as i64).into()).collect();
                m.insert(key.into(), s.into());
            }
        }
        m.insert("scale".into(), scale.into());
        scaled.insert(k.clone(), m.into());
    }
    std::fs::write(out.join("index.json"), serde_json::to_string_pretty(&scaled).unwrap()).expect("index write");
    println!("wrote {} images to {}", handles.len() - failed.len(), out.display());
    for f in &failed {
        eprintln!("failed: {f}");
    }
}
