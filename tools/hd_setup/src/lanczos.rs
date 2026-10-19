//! Lanczos resize of straight RGBA, done the way Pillow's `Image.resize(..., LANCZOS)` does it for
//! RGBA (premultiply, separable 8-bit fixed-point passes, horizontal first, unpremultiply), so the
//! HD textures match the ones upscale_sprites.py made.

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let x = x * std::f64::consts::PI;
        x.sin() / x
    }
}

fn lanczos(x: f64) -> f64 {
    if (-3.0..3.0).contains(&x) {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// Per output pixel: first input index and fixed-point weights.
fn coeffs(in_size: usize, out_size: usize) -> Vec<(usize, Vec<i32>)> {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = 3.0 * filterscale;
    (0..out_size)
        .map(|xx| {
            let center = (xx as f64 + 0.5) * scale;
            let ss = 1.0 / filterscale;
            let xmin = ((center - support + 0.5) as i64).max(0) as usize;
            let xmax = ((center + support + 0.5) as i64).min(in_size as i64) as usize - xmin;
            let k: Vec<f64> = (0..xmax).map(|x| lanczos((x as f64 + xmin as f64 - center + 0.5) * ss)).collect();
            let ww: f64 = k.iter().sum();
            let one = (1u32 << PRECISION_BITS) as f64;
            let kk = k
                .iter()
                .map(|&w| {
                    let w = if ww != 0.0 { w / ww } else { w };
                    if w < 0.0 { (-0.5 + w * one) as i32 } else { (0.5 + w * one) as i32 }
                })
                .collect();
            (xmin, kk)
        })
        .collect()
}

fn clip8(ss: i64) -> u8 {
    (ss >> PRECISION_BITS).clamp(0, 255) as u8
}

fn premultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for p in out.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 {
            p[..3].fill(0);
        } else if a != 255 {
            for c in &mut p[..3] {
                let t = *c as u32 * a + 128;
                *c = (((t >> 8) + t) >> 8) as u8;
            }
        }
    }
    out
}

fn unpremultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = (255 * *c as u32 / a).min(255) as u8;
            }
        }
    }
}

pub fn resize_rgba(src: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<u8> {
    let pm = premultiply(src);
    let half = 1i64 << (PRECISION_BITS - 1);
    // Horizontal pass: w x h -> ow x h.
    let cx = coeffs(w, ow);
    let mut tmp = vec![0u8; ow * h * 4];
    for y in 0..h {
        let row = &pm[y * w * 4..(y + 1) * w * 4];
        for (xx, (xmin, k)) in cx.iter().enumerate() {
            for c in 0..4 {
                let mut ss = half;
                for (i, &kv) in k.iter().enumerate() {
                    ss += row[(xmin + i) * 4 + c] as i64 * kv as i64;
                }
                tmp[(y * ow + xx) * 4 + c] = clip8(ss);
            }
        }
    }
    // Vertical pass: ow x h -> ow x oh.
    let cy = coeffs(h, oh);
    let mut out = vec![0u8; ow * oh * 4];
    for (yy, (ymin, k)) in cy.iter().enumerate() {
        for x in 0..ow {
            for c in 0..4 {
                let mut ss = half;
                for (i, &kv) in k.iter().enumerate() {
                    ss += tmp[((ymin + i) * ow + x) * 4 + c] as i64 * kv as i64;
                }
                out[(yy * ow + x) * 4 + c] = clip8(ss);
            }
        }
    }
    unpremultiply(&mut out);
    out
}
