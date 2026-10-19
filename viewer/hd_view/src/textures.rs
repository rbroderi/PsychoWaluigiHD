//! Image sources and the GPU texture cache.
//!
//! Every bank image has an original (sprites/) and HD versions in one or more art styles (xBRZ,
//! cleanEdge). Originals are small and decoded on the render thread on first use; HD PNGs are
//! decoded on a worker thread and swapped in when ready, so a missing or slow HD texture only
//! means the original is shown for a few frames. Textures of every style stay cached (within the
//! memory budget), so switching style is instant once its art has been seen.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

/// Size and hot spot of a bank image at original resolution.
#[derive(Clone, Copy)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
}

pub fn load_index(dir: &Path) -> Result<HashMap<u32, ImageInfo>, String> {
    let text = std::fs::read_to_string(dir.join("index.json")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    for (k, v) in json.as_object().ok_or("index.json is not an object")? {
        let (Ok(handle), Some(w), Some(h), Some(hot)) =
            (k.parse(), v["width"].as_u64(), v["height"].as_u64(), v["hotspot"].as_array())
        else {
            continue;
        };
        out.insert(handle, ImageInfo {
            width: w as u32,
            height: h as u32,
            hot_x: hot[0].as_i64().unwrap_or(0) as i32,
            hot_y: hot[1].as_i64().unwrap_or(0) as i32,
        });
    }
    Ok(out)
}

/// Decodes a PNG to premultiplied RGBA.
pub fn decode_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(File::open(path).ok()?);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    let mut rgba = rgba;
    for p in rgba.chunks_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    Some((info.width, info.height, rgba))
}

pub struct Texture {
    pub srv: ID3D11ShaderResourceView,
    pub bytes: usize,
    pub last_used: u64,
}

pub fn create_texture(device: &ID3D11Device, w: u32, h: u32, rgba: &[u8]) -> Option<Texture> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: rgba.as_ptr() as _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
    unsafe {
        let mut tex = None;
        device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)).ok()?;
        let mut srv = None;
        device.CreateShaderResourceView(&tex?, None, Some(&mut srv)).ok()?;
        Some(Texture { srv: srv?, bytes: (w * h * 4) as usize, last_used: 0 })
    }
}

struct Job {
    bank: u32,
    style: usize,
    path: PathBuf,
}

type Done = (u32, usize, Option<(u32, u32, Vec<u8>)>);

/// An HD art set the viewer can show: a name for the title bar and its folder.
pub struct Style {
    pub name: String,
    dir: PathBuf,
}

pub struct Cache {
    pub info: HashMap<u32, ImageInfo>,
    sprites_dir: PathBuf,
    styles: Vec<Style>,
    /// Which style `get` serves.
    pub style: usize,
    originals: HashMap<u32, Option<Texture>>,
    hd: HashMap<(u32, usize), Texture>,
    hd_missing: HashSet<(u32, usize)>,
    pending: HashSet<(u32, usize)>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    pub budget: usize,
    pub hd_bytes: usize,
    clock: u64,
}

impl Cache {
    /// `styles`: (name, folder) of each HD art set; ones whose folder is missing are left out.
    pub fn new(sprites_dir: PathBuf, styles: Vec<(String, PathBuf)>, budget: usize) -> Result<Cache, String> {
        let info = load_index(&sprites_dir)?;
        let (jobs, job_rx) = channel::<Job>();
        let (done_tx, done) = channel::<Done>();
        // Two decoder threads sharing one queue keep level loads quick without starving the game.
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        for _ in 0..2 {
            let rx = job_rx.clone();
            let tx = done_tx.clone();
            std::thread::spawn(move || loop {
                let job = match rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let _ = tx.send((job.bank, job.style, decode_png(&job.path)));
            });
        }
        let styles = styles.into_iter().filter(|(_, dir)| dir.is_dir()).map(|(name, dir)| Style { name, dir }).collect();
        Ok(Cache {
            info,
            sprites_dir,
            styles,
            style: 0,
            originals: HashMap::new(),
            hd: HashMap::new(),
            hd_missing: HashSet::new(),
            pending: HashSet::new(),
            jobs,
            done,
            budget,
            hd_bytes: 0,
            clock: 0,
        })
    }

    pub fn styles(&self) -> &[Style] {
        &self.styles
    }

    pub fn begin_frame(&mut self, device: &ID3D11Device) {
        self.clock += 1;
        while let Ok((bank, style, img)) = self.done.try_recv() {
            self.pending.remove(&(bank, style));
            match img.and_then(|(w, h, px)| create_texture(device, w, h, &px)) {
                Some(mut t) => {
                    t.last_used = self.clock;
                    self.hd_bytes += t.bytes;
                    self.hd.insert((bank, style), t);
                }
                None => {
                    self.hd_missing.insert((bank, style));
                }
            }
        }
        if self.hd_bytes > self.budget {
            let mut by_age: Vec<_> = self.hd.iter().map(|(k, t)| (t.last_used, *k)).collect();
            by_age.sort();
            for (used, key) in by_age {
                if self.hd_bytes <= self.budget * 3 / 4 || used + 2 >= self.clock {
                    break;
                }
                if let Some(t) = self.hd.remove(&key) {
                    self.hd_bytes -= t.bytes;
                }
            }
        }
    }

    /// The current style's HD texture if loaded (queuing it if not), else the original.
    /// Returns (view, is_hd).
    pub fn get(&mut self, device: &ID3D11Device, bank: u32) -> Option<(ID3D11ShaderResourceView, bool)> {
        let key = (bank, self.style);
        if let Some(t) = self.hd.get_mut(&key) {
            t.last_used = self.clock;
            return Some((t.srv.clone(), true));
        }
        if let Some(style) = self.styles.get(self.style) {
            if !self.hd_missing.contains(&key) && self.pending.insert(key) {
                let _ = self.jobs.send(Job { bank, style: self.style, path: style.dir.join(format!("{bank:05}.png")) });
            }
        }
        self.original(device, bank).map(|srv| (srv, false))
    }

    /// The original (low-res) image, as the game drew it.
    pub fn original(&mut self, device: &ID3D11Device, bank: u32) -> Option<ID3D11ShaderResourceView> {
        let dir = &self.sprites_dir;
        let orig = self.originals.entry(bank).or_insert_with(|| {
            decode_png(&dir.join(format!("{bank:05}.png"))).and_then(|(w, h, px)| create_texture(device, w, h, &px))
        });
        orig.as_ref().map(|t| t.srv.clone())
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.hd.len(), self.pending.len(), self.hd_bytes)
    }
}
