//! D3D11 renderer: draws a frame snapshot into a scene target `scale` times the game's
//! resolution, then letterboxes that target into the window.
//!
//! The level background is rebuilt from the background ops (pastes, tile blits, fills) the proxy
//! reports. To know where that rebuild is trustworthy, the ops are first drawn at 1x with the
//! original art into a reconstruction target and compared with the game's background surface.
//!
//! Layers, back to front:
//!   1. base: the game's screen where the ops don't cover it, point-scaled. Under pixels the ops
//!      reproduce, the colour of an uncovered neighbour, so the transparent corners of smooth HD
//!      edges show what is really behind them rather than the low-res stair-steps
//!   2. background ops in HD, in order: images (smooth-edged art), gradient and flat fills
//!   3. residual: covered pixels outside every sprite box that the ops don't reproduce (a backdrop
//!      piece went stale, or the game drew over it some way we don't track): the game's pixels
//!      win. Magenta in the F7 debug view
//!   4. sprites, HD; owner-draw sprites and anything without an image are cut out of the
//!      low-res screen (only pixels that differ from the background, when we have it)
//! Without a background surface: the whole low-res screen, then ops and sprites.

use std::path::Path;

use hdshared::{BackOp, Sprite, BOP_ADD, BOP_BLEND, BOP_COPY, BOP_SUB, EFFECT_MASK, KIND_IMAGE, NO_BANK, OP_BLIT,
               OP_FLAT, OP_GRADIENT, OP_PASTE, OP_TILE, SPRITE_HIDDEN};
use windows::core::{s, Interface, PCSTR};
use windows::Win32::Foundation::{BOOL, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::shm::Snapshot;
use crate::textures::Cache;

const SHADERS: &str = r#"
Texture2D t0 : register(t0);
Texture2D t1 : register(t1);
// Sprite boxes this frame (low-res; red > 0.5 inside a visible sprite or owner-drawn object).
Texture2D t2 : register(t2);
// The game's background surface (or the screen again when there is none).
Texture2D t3 : register(t3);
SamplerState s0 : register(s0);
struct V { float2 p : POSITION; float2 uv : TEXCOORD0; float4 c : COLOR0; };
struct P { float4 p : SV_POSITION; float2 uv : TEXCOORD0; float4 c : COLOR0; };
P vs(V v) { P o; o.p = float4(v.p, 0, 1); o.uv = v.uv; o.c = v.c; return o; }
// Premultiplied texture, tinted/faded by the vertex colour.
float4 ps_tex(P i) : SV_Target { return t0.Sample(s0, i.uv) * i.c; }
// Untextured: just the (premultiplied) vertex colour.
float4 ps_color(P i) : SV_Target { return i.c; }
// Opaque copy of the low-res screen/background (alpha channel there is unused).
float4 ps_opaque(P i) : SV_Target { return float4(t0.Sample(s0, i.uv).rgb, 1) * i.c; }
// Low-res screen pixels that differ from the background: the owner-drawn foreground.
float4 ps_cutout(P i) : SV_Target {
    float3 s = t0.Sample(s0, i.uv).rgb;
    float3 b = t1.Sample(s0, i.uv).rgb;
    float a = any(abs(s - b) > 1.0 / 512.0) ? 1.0 : 0.0;
    return float4(s * a, a) * i.c;
}
// t0 = the game's screen (low-res), t1 = 1x reconstruction from the background ops.
// The vertex colour's green channel is 0 in the residual debug view.
static const float TOL = 10.0 / 255.0;
bool reproduced(int2 p) {
    float4 r = t1.Load(int3(p, 0));
    return r.a > 0.5 && all(abs(t0.Load(int3(p, 0)).rgb - r.rgb) <= TOL);
}
int2 texel(P i) {
    uint w, h;
    t0.GetDimensions(w, h);
    return min(int2(i.uv * float2(w, h)), int2(w - 1, h - 1));
}
// What the base layer shows at an uncovered pixel: the game's screen, except inside sprite boxes,
// where the screen has the sprite in it (drawn again in HD on top), so the background surface.
float3 uncovered(int2 p) {
    return t2.Load(int3(p, 0)).r > 0.5 ? t3.Load(int3(p, 0)).rgb : t0.Load(int3(p, 0)).rgb;
}
float4 ps_base(P i) : SV_Target {
    int2 p = texel(i);
    float3 b = uncovered(p);
    bool debug = i.c.g < 0.5;
    if (t1.Load(int3(p, 0)).a < 0.5)
        return float4(debug ? lerp(b, float3(0, 1, 1), 0.5) : b, 1);
    if (!reproduced(p))
        return float4(b, 1);  // the HD ops draw over it
    // Reproduced by the ops: lend the colour of an uncovered neighbour to sit under HD edges.
    uint w, h;
    t0.GetDimensions(w, h);
    static const int2 n[8] = { int2(-1, 0), int2(1, 0), int2(0, -1), int2(0, 1),
                               int2(-1, -1), int2(1, -1), int2(-1, 1), int2(1, 1) };
    [unroll] for (int k = 0; k < 8; k++) {
        int2 q = clamp(p + n[k], int2(0, 0), int2(w - 1, h - 1));
        if (t1.Load(int3(q, 0)).a < 0.5)
            return float4(uncovered(q), 1);
    }
    return float4(0, 0, 0, 0);
}
float4 ps_residual(P i) : SV_Target {
    int2 p = texel(i);
    // Only where the rebuild is provably wrong: outside every sprite box the game's screen is
    // background, so if the ops show something else there, one of them is stale or the game drew
    // over it some way we don't track.
    if (t1.Load(int3(p, 0)).a < 0.5 || reproduced(p) || t2.Load(int3(p, 0)).r > 0.5)
        return float4(0, 0, 0, 0);
    return i.c.g < 0.5 ? float4(1, 0, 1, 1) : float4(t0.Load(int3(p, 0)).rgb, 1);
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    col: [f32; 4],
}

#[derive(Clone, Copy, PartialEq)]
enum Shader {
    Tex,
    Color,
    Opaque,
    Cutout,
    Base,
    Residual,
}

#[derive(Clone, Copy, PartialEq)]
enum Blend {
    Premul,
    Add,
    Sub,
}

struct Cmd {
    first: u32,
    count: u32,
    shader: Shader,
    blend: Blend,
    linear: bool,
    srv0: ID3D11ShaderResourceView,
    srv1: Option<ID3D11ShaderResourceView>,
}

/// What the scene target shows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Hd,
    Original,
}

#[derive(Default)]
pub struct Stats {
    pub hd_draws: u32,
    pub fallback_draws: u32,
    /// Draw calls issued (reconstruction + scene).
    pub draw_calls: u32,
    /// CPU time to build and submit the frame, and time spent in Present (GPU + vsync wait).
    pub build_ms: f32,
    pub present_ms: f32,
}

struct LowRes {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
}

pub struct Gfx {
    pub device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    back_rtv: Option<ID3D11RenderTargetView>,
    window: (u32, u32),
    scene: Option<(ID3D11Texture2D, ID3D11RenderTargetView, ID3D11ShaderResourceView, u32, u32)>,
    pub scale: u32,
    vs: ID3D11VertexShader,
    ps_tex: ID3D11PixelShader,
    ps_color: ID3D11PixelShader,
    ps_opaque: ID3D11PixelShader,
    ps_cutout: ID3D11PixelShader,
    ps_base: ID3D11PixelShader,
    ps_residual: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    vb: ID3D11Buffer,
    vb_capacity: usize,
    blend_premul: ID3D11BlendState,
    blend_add: ID3D11BlendState,
    blend_sub: ID3D11BlendState,
    point: ID3D11SamplerState,
    linear: ID3D11SamplerState,
    screen: Option<LowRes>,
    back: Option<LowRes>,
    /// 1x reconstruction of the background from the ops: (RTV, SRV).
    recon: Option<(ID3D11RenderTargetView, ID3D11ShaderResourceView)>,
    /// Where this frame's sprites are (shader slot t2), so the residual leaves them alone.
    sprite_mask: Option<LowRes>,
    /// Whether `back` holds this frame's background surface (else the residual uses the screen).
    back_valid: bool,
    low_size: (u32, u32),
    /// Show background pixels the ops leave uncovered (cyan) or don't reproduce (magenta).
    pub debug_residual: bool,
}

fn compile(entry: PCSTR, target: PCSTR) -> windows::core::Result<Vec<u8>> {
    unsafe {
        let mut blob = None;
        let mut err = None;
        let r = D3DCompile(SHADERS.as_ptr() as _, SHADERS.len(), s!("hd_view"), None, None, entry, target, 0, 0,
                           &mut blob, Some(&mut err));
        if let Err(e) = r {
            if let Some(err) = err {
                let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
                eprintln!("shader error: {}", String::from_utf8_lossy(msg));
            }
            return Err(e);
        }
        let blob = blob.unwrap();
        Ok(std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()).to_vec())
    }
}

fn blend_state(device: &ID3D11Device, src: D3D11_BLEND, dst: D3D11_BLEND, op: D3D11_BLEND_OP)
    -> windows::core::Result<ID3D11BlendState> {
    let rt = D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: BOOL(1),
        SrcBlend: src,
        DestBlend: dst,
        BlendOp: op,
        SrcBlendAlpha: D3D11_BLEND_ONE,
        DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
        BlendOpAlpha: D3D11_BLEND_OP_ADD,
        RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };
    let desc = D3D11_BLEND_DESC { AlphaToCoverageEnable: BOOL(0), IndependentBlendEnable: BOOL(0), RenderTarget: [rt; 8] };
    let mut out = None;
    unsafe { device.CreateBlendState(&desc, Some(&mut out))? };
    Ok(out.unwrap())
}

fn sampler(device: &ID3D11Device, filter: D3D11_FILTER) -> windows::core::Result<ID3D11SamplerState> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: filter,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut out = None;
    unsafe { device.CreateSamplerState(&desc, Some(&mut out))? };
    Ok(out.unwrap())
}

impl Gfx {
    pub fn new(hwnd: HWND, width: u32, height: u32, scale: u32) -> windows::core::Result<Gfx> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                              Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None,
                              Some(&mut ctx))?;
            let device: ID3D11Device = device.unwrap();
            let ctx = ctx.unwrap();

            let dxgi: IDXGIDevice = device.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;

            let vs_bytes = compile(s!("vs"), s!("vs_5_0"))?;
            let mut vs = None;
            device.CreateVertexShader(&vs_bytes, None, Some(&mut vs))?;
            let ps = |entry| -> windows::core::Result<ID3D11PixelShader> {
                let bytes = compile(entry, s!("ps_5_0"))?;
                let mut out = None;
                device.CreatePixelShader(&bytes, None, Some(&mut out))?;
                Ok(out.unwrap())
            };
            let (ps_tex, ps_color, ps_opaque, ps_cutout) =
                (ps(s!("ps_tex"))?, ps(s!("ps_color"))?, ps(s!("ps_opaque"))?, ps(s!("ps_cutout"))?);
            let (ps_base, ps_residual) = (ps(s!("ps_base"))?, ps(s!("ps_residual"))?);

            let elem = |name: PCSTR, fmt, off| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: off,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let elements = [
                elem(s!("POSITION"), DXGI_FORMAT_R32G32_FLOAT, 0),
                elem(s!("TEXCOORD"), DXGI_FORMAT_R32G32_FLOAT, 8),
                elem(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, 16),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, &vs_bytes, Some(&mut layout))?;

            let mut gfx = Gfx {
                vb: Self::vertex_buffer(&device, 6 * 8192)?,
                vb_capacity: 6 * 8192,
                blend_premul: blend_state(&device, D3D11_BLEND_ONE, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_OP_ADD)?,
                blend_add: blend_state(&device, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD)?,
                blend_sub: blend_state(&device, D3D11_BLEND_ONE, D3D11_BLEND_ONE, D3D11_BLEND_OP_REV_SUBTRACT)?,
                point: sampler(&device, D3D11_FILTER_MIN_MAG_MIP_POINT)?,
                linear: sampler(&device, D3D11_FILTER_MIN_MAG_MIP_LINEAR)?,
                device,
                ctx,
                swap,
                back_rtv: None,
                window: (width, height),
                scene: None,
                scale,
                vs: vs.unwrap(),
                ps_tex,
                ps_color,
                ps_opaque,
                ps_cutout,
                ps_base,
                ps_residual,
                layout: layout.unwrap(),
                screen: None,
                back: None,
                recon: None,
                sprite_mask: None,
                back_valid: false,
                low_size: (0, 0),
                debug_residual: false,
            };
            gfx.make_back_rtv()?;
            Ok(gfx)
        }
    }

    fn vertex_buffer(device: &ID3D11Device, vertices: usize) -> windows::core::Result<ID3D11Buffer> {
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: (vertices * std::mem::size_of::<Vertex>()) as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut out = None;
        unsafe { device.CreateBuffer(&desc, None, Some(&mut out))? };
        Ok(out.unwrap())
    }

    fn make_back_rtv(&mut self) -> windows::core::Result<()> {
        unsafe {
            let buf: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(&buf, None, Some(&mut rtv))?;
            self.back_rtv = rtv;
        }
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> windows::core::Result<()> {
        if width == 0 || height == 0 || (width, height) == self.window {
            return Ok(());
        }
        self.back_rtv = None;
        unsafe {
            self.ctx.OMSetRenderTargets(None, None);
            self.swap.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
        }
        self.window = (width, height);
        self.make_back_rtv()
    }

    fn ensure_scene(&mut self, w: u32, h: u32) -> windows::core::Result<()> {
        if matches!(self.scene, Some((_, _, _, sw, sh)) if (sw, sh) == (w, h)) {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        unsafe {
            let mut tex = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = tex.unwrap();
            let (mut rtv, mut srv) = (None, None);
            self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            self.scene = Some((tex, rtv.unwrap(), srv.unwrap(), w, h));
        }
        Ok(())
    }

    fn render_target(&self, w: u32, h: u32) -> windows::core::Result<(ID3D11RenderTargetView, ID3D11ShaderResourceView)> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        unsafe {
            let mut tex = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = tex.unwrap();
            let (mut rtv, mut srv) = (None, None);
            self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            Ok((rtv.unwrap(), srv.unwrap()))
        }
    }

    /// Geometry for the background ops. `hd` picks HD art (scene pass) or the original art at
    /// point sampling (1x reconstruction pass).
    fn back_op_cmds(&self, snap: &Snapshot, cache: &mut Cache, ndc: &dyn Fn(f32, f32) -> [f32; 2], hd_pass: bool,
                    filler: &ID3D11ShaderResourceView, verts: &mut Vec<Vertex>, cmds: &mut Vec<Cmd>, stats: &mut Stats) {
        for op in &snap.back_ops {
            let Some((blend, col)) = ink(op.effect, op.param) else { continue };
            match op.kind {
                OP_PASTE | OP_TILE | OP_BLIT => {
                    let Some(info) = cache.info.get(&op.bank).copied() else { continue };
                    let tex = if hd_pass {
                        cache.get(&self.device, op.bank)
                    } else {
                        cache.original(&self.device, op.bank).map(|t| (t, false))
                    };
                    let Some((srv, hd)) = tex else { continue };
                    let (tw, th) = (info.width as f32, info.height as f32);
                    let rects = match op.kind {
                        OP_PASTE => vec![(op.x as f32, op.y as f32, tw, th, [0.0, 0.0, 1.0, 1.0])],
                        OP_BLIT => {
                            let (sx, sy, w, h) = (op.org_x as f32, op.org_y as f32, op.w as f32, op.h as f32);
                            vec![(op.x as f32, op.y as f32, w, h, [sx / tw, sy / th, (sx + w) / tw, (sy + h) / th])]
                        }
                        _ => tile_rects(op, tw, th),
                    };
                    for (x, y, w, h, uv) in rects {
                        let first = push_quad(verts, [ndc(x, y), ndc(x + w, y), ndc(x, y + h), ndc(x + w, y + h)], uv, col);
                        cmds.push(Cmd { first, count: 6, shader: Shader::Tex, blend, linear: hd, srv0: srv.clone(), srv1: None });
                    }
                    if hd_pass {
                        if hd { stats.hd_draws += 1 } else { stats.fallback_draws += 1 }
                    }
                }
                OP_GRADIENT | OP_FLAT => {
                    let (x, y, w, h) = (op.x as f32, op.y as f32, op.w as f32, op.h as f32);
                    let c1 = colorref(op.color1, col);
                    let c2 = if op.kind == OP_GRADIENT { colorref(op.color2, col) } else { c1 };
                    // CFillDir runs from color1 to color2 across the rectangle.
                    let (tl, tr, bl, br) = if op.vertical != 0 { (c1, c1, c2, c2) } else { (c1, c2, c1, c2) };
                    let first = push_quad_colors(verts, [ndc(x, y), ndc(x + w, y), ndc(x, y + h), ndc(x + w, y + h)],
                                                 [tl, tr, bl, br]);
                    cmds.push(Cmd { first, count: 6, shader: Shader::Color, blend, linear: false, srv0: filler.clone(), srv1: None });
                    if hd_pass {
                        stats.hd_draws += 1;
                    }
                }
                _ => {}
            }
        }
    }

    fn low_res(&self, w: u32, h: u32) -> windows::core::Result<LowRes> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        unsafe {
            let mut tex = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = tex.unwrap();
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            Ok(LowRes { tex, srv: srv.unwrap() })
        }
    }

    fn upload_low_res(&mut self, snap: &Snapshot) -> windows::core::Result<()> {
        let (w, h) = (snap.width, snap.height);
        if self.low_size != (w, h) || self.screen.is_none() {
            self.screen = Some(self.low_res(w, h)?);
            self.back = Some(self.low_res(w, h)?);
            self.recon = Some(self.render_target(w, h)?);
            self.sprite_mask = Some(self.low_res(w, h)?);
            self.low_size = (w, h);
        }
        unsafe {
            self.ctx.UpdateSubresource(&self.screen.as_ref().unwrap().tex, 0, None, snap.screen.as_ptr() as _, w * 4, 0);
            if snap.has_back {
                self.ctx.UpdateSubresource(&self.back.as_ref().unwrap().tex, 0, None, snap.back.as_ptr() as _, w * 4, 0);
            }
            self.back_valid = snap.has_back;
            let mask = sprite_mask(snap);
            self.ctx.UpdateSubresource(&self.sprite_mask.as_ref().unwrap().tex, 0, None, mask.as_ptr() as _, w * 4, 0);
        }
        Ok(())
    }

    /// Renders `snap` into the scene target and presents it letterboxed in the window.
    pub fn draw(&mut self, snap: &Snapshot, cache: &mut Cache, view: View) -> windows::core::Result<Stats> {
        let started = std::time::Instant::now();
        let mut stats = Stats::default();
        let (lw, lh) = (snap.width, snap.height);
        let have_frame = lw > 0 && lh > 0;
        let (sw, sh) = if have_frame { (lw * self.scale, lh * self.scale) } else { (320 * self.scale, 240 * self.scale) };
        self.ensure_scene(sw, sh)?;
        let mut verts: Vec<Vertex> = Vec::new();
        let mut cmds: Vec<Cmd> = Vec::new();
        let mut recon_verts: Vec<Vertex> = Vec::new();
        let mut recon_cmds: Vec<Cmd> = Vec::new();
        let rebuild = have_frame && snap.has_back && view == View::Hd;
        // Some levels draw their backdrops straight onto the screen and never refresh the
        // background surface, which then still shows an earlier level. A current background
        // matches the screen everywhere sprites and text boxes don't cover; a stale one hardly
        // anywhere. When stale, the screen stands in for it and the rebuild is trusted.

        if have_frame {
            self.upload_low_res(snap)?;
            cache.begin_frame(&self.device);
            let screen = self.screen.as_ref().unwrap().srv.clone();
            let back = self.back.as_ref().unwrap().srv.clone();
            let s = self.scale as f32;
            let (ox, oy) = (snap.origin.0 as f32, snap.origin.1 as f32);
            // Logical coordinates -> scene NDC.
            let ndc = |x: f32, y: f32| [((x - ox) * s) / sw as f32 * 2.0 - 1.0, 1.0 - ((y - oy) * s) / sh as f32 * 2.0];
            let full = |verts: &mut Vec<Vertex>| push_quad(verts, [ndc(ox, oy), ndc(ox + lw as f32, oy), ndc(ox, oy + lh as f32),
                ndc(ox + lw as f32, oy + lh as f32)], [0.0, 0.0, 1.0, 1.0], [1.0; 4]);

            let debug_col = if self.debug_residual { [1.0, 0.0, 1.0, 1.0] } else { [1.0; 4] };
            if rebuild {
                let recon = self.recon.as_ref().unwrap().1.clone();
                self.back_op_cmds(snap, cache, &ndc, false, &screen, &mut recon_verts, &mut recon_cmds, &mut stats);
                let first = push_quad(&mut verts, [ndc(ox, oy), ndc(ox + lw as f32, oy), ndc(ox, oy + lh as f32),
                                                   ndc(ox + lw as f32, oy + lh as f32)], [0.0, 0.0, 1.0, 1.0], debug_col);
                cmds.push(Cmd { first, count: 6, shader: Shader::Base, blend: Blend::Premul, linear: false,
                                srv0: screen.clone(), srv1: Some(recon.clone()) });
                self.back_op_cmds(snap, cache, &ndc, true, &screen, &mut verts, &mut cmds, &mut stats);
                {
                    cmds.push(Cmd { first, count: 6, shader: Shader::Residual, blend: Blend::Premul, linear: false,
                                    srv0: screen.clone(), srv1: Some(recon) });
                }
            } else {
                let first = full(&mut verts);
                cmds.push(Cmd { first, count: 6, shader: Shader::Opaque, blend: Blend::Premul, linear: false,
                                srv0: screen.clone(), srv1: None });
                if view == View::Hd {
                    self.back_op_cmds(snap, cache, &ndc, true, &screen, &mut verts, &mut cmds, &mut stats);
                }
            }

            if view == View::Hd {

                for spr in snap.sprites.iter().filter(|s| s.flags & SPRITE_HIDDEN == 0) {
                    let image = if spr.kind == KIND_IMAGE && spr.bank != NO_BANK {
                        cache.info.get(&spr.bank).copied().zip(ink(spr.effect, spr.param))
                    } else {
                        None
                    };
                    match image.and_then(|(info, ink)| cache.get(&self.device, spr.bank).map(|t| (info, ink, t))) {
                        Some((info, (blend, col), (srv, hd))) => {
                            let corners = sprite_corners(spr, info.width as f32, info.height as f32, info.hot_x as f32,
                                                         info.hot_y as f32).map(|(x, y)| ndc(x, y));
                            let first = push_quad(&mut verts, corners, [0.0, 0.0, 1.0, 1.0], col);
                            cmds.push(Cmd { first, count: 6, shader: Shader::Tex, blend, linear: hd, srv0: srv, srv1: None });
                            if hd { stats.hd_draws += 1 } else { stats.fallback_draws += 1 }
                        }
                        None => {
                            // Owner-draw, unknown image or unsupported ink: cut the sprite's box out of
                            // the low-res screen.
                            let (x1, y1, x2, y2) = (spr.x1.max(snap.origin.0), spr.y1.max(snap.origin.1),
                                                    spr.x2.min(snap.origin.0 + lw as i32), spr.y2.min(snap.origin.1 + lh as i32));
                            if x2 <= x1 || y2 <= y1 {
                                continue;
                            }
                            let (x1, y1, x2, y2) = (x1 as f32, y1 as f32, x2 as f32, y2 as f32);
                            let uv = [(x1 - ox) / lw as f32, (y1 - oy) / lh as f32, (x2 - ox) / lw as f32, (y2 - oy) / lh as f32];
                            let first = push_quad(&mut verts, [ndc(x1, y1), ndc(x2, y1), ndc(x1, y2), ndc(x2, y2)], uv, [1.0; 4]);
                            let (shader, srv1) = if snap.has_back { (Shader::Cutout, Some(back.clone())) } else { (Shader::Opaque, None) };
                            cmds.push(Cmd { first, count: 6, shader, blend: Blend::Premul, linear: false, srv0: screen.clone(), srv1 });
                            stats.fallback_draws += 1;
                        }
                    }
                }
            }
        }

        unsafe {
            if rebuild {
                let (recon_rtv, _) = self.recon.clone().unwrap();
                self.ctx.ClearRenderTargetView(&recon_rtv, &[0.0; 4]);
                self.run(&recon_rtv, (lw, lh), (0.0, 0.0, lw as f32, lh as f32), &recon_verts, &recon_cmds)?;
            }
            let (_, scene_rtv, scene_srv, _, _) = self.scene.clone().unwrap();
            self.ctx.ClearRenderTargetView(&scene_rtv, &[0.0, 0.0, 0.0, 1.0]);
            self.run(&scene_rtv, (sw, sh), (0.0, 0.0, sw as f32, sh as f32), &verts, &cmds)?;

            // Present: letterbox the scene into the window, keeping its aspect ratio.
            let (ww, wh) = (self.window.0 as f32, self.window.1 as f32);
            let k = (ww / sw as f32).min(wh / sh as f32);
            let (vw, vh) = (sw as f32 * k, sh as f32 * k);
            let mut pv = Vec::new();
            push_quad(&mut pv, [[-1.0, 1.0], [1.0, 1.0], [-1.0, -1.0], [1.0, -1.0]], [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
            let pc = [Cmd { first: 0, count: 6, shader: Shader::Opaque, blend: Blend::Premul, linear: true,
                            srv0: scene_srv, srv1: None }];
            let back_rtv = self.back_rtv.clone().unwrap();
            self.ctx.ClearRenderTargetView(&back_rtv, &[0.0, 0.0, 0.0, 1.0]);
            self.run(&back_rtv, self.window, ((ww - vw) / 2.0, (wh - vh) / 2.0, vw, vh), &pv, &pc)?;
            stats.draw_calls = (recon_cmds.len() + cmds.len()) as u32;
            stats.build_ms = started.elapsed().as_secs_f32() * 1000.0;
            let presenting = std::time::Instant::now();
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
            stats.present_ms = presenting.elapsed().as_secs_f32() * 1000.0;
        }
        Ok(stats)
    }

    unsafe fn run(&mut self, rtv: &ID3D11RenderTargetView, _size: (u32, u32), vp: (f32, f32, f32, f32),
                  verts: &[Vertex], cmds: &[Cmd]) -> windows::core::Result<()> {
        if verts.is_empty() {
            return Ok(());
        }
        if verts.len() > self.vb_capacity {
            self.vb_capacity = verts.len().next_power_of_two();
            self.vb = Self::vertex_buffer(&self.device, self.vb_capacity)?;
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        self.ctx.Map(&self.vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
        std::ptr::copy_nonoverlapping(verts.as_ptr(), mapped.pData as *mut Vertex, verts.len());
        self.ctx.Unmap(&self.vb, 0);

        let ctx = &self.ctx;
        ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
        ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: vp.0, TopLeftY: vp.1, Width: vp.2, Height: vp.3,
                                                   MinDepth: 0.0, MaxDepth: 1.0 }]));
        ctx.IASetInputLayout(&self.layout);
        ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        let stride = std::mem::size_of::<Vertex>() as u32;
        ctx.IASetVertexBuffers(0, 1, Some(&Some(self.vb.clone())), Some(&stride), Some(&0));
        ctx.VSSetShader(&self.vs, None);
        for c in cmds {
            let ps = match c.shader {
                Shader::Tex => &self.ps_tex,
                Shader::Color => &self.ps_color,
                Shader::Opaque => &self.ps_opaque,
                Shader::Cutout => &self.ps_cutout,
                Shader::Base => &self.ps_base,
                Shader::Residual => &self.ps_residual,
            };
            ctx.PSSetShader(ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(c.srv0.clone()), c.srv1.clone()]));
            if c.shader == Shader::Residual || c.shader == Shader::Base {
                let back = if self.back_valid { &self.back } else { &self.screen };
                ctx.PSSetShaderResources(2, Some(&[self.sprite_mask.as_ref().map(|m| m.srv.clone()),
                                                   back.as_ref().map(|b| b.srv.clone())]));
            }
            ctx.PSSetSamplers(0, Some(&[Some(if c.linear { self.linear.clone() } else { self.point.clone() })]));
            let blend = match c.blend {
                Blend::Premul => &self.blend_premul,
                Blend::Add => &self.blend_add,
                Blend::Sub => &self.blend_sub,
            };
            ctx.OMSetBlendState(blend, Some(&[0.0; 4]), 0xFFFF_FFFF);
            ctx.Draw(c.count, c.first);
        }
        ctx.PSSetShaderResources(0, Some(&[None, None, None, None]));
        Ok(())
    }

    /// How much of the last frame the residual pass replaced with the game's low-res pixels:
    /// (share of the screen, bounding box x1, y1, x2, y2 in game pixels). Reads the 1x
    /// reconstruction back from the GPU, so only for occasional diagnostics (--stats).
    pub fn residual_share(&self, snap: &Snapshot) -> Option<(f32, [i32; 4])> {
        let (w, h) = (snap.width as usize, snap.height as usize);
        if w == 0 || !snap.has_back || snap.screen.len() < w * h {
            return None;
        }
        let (rtv, _) = self.recon.clone()?;
        unsafe {
            let tex: ID3D11Texture2D = rtv.GetResource().ok()?.cast().ok()?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w as u32,
                Height: h as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging)).ok()?;
            let staging = staging?;
            self.ctx.CopyResource(&staging, &tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
            let mask = sprite_mask(snap);
            let (mut n, mut bb) = (0usize, [i32::MAX, i32::MAX, i32::MIN, i32::MIN]);
            let tol = 10i32;
            for y in 0..h {
                let row = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w * 4);
                for x in 0..w {
                    let r = &row[x * 4..x * 4 + 4];
                    if r[3] < 128 || mask[y * w + x] != 0 {
                        continue;
                    }
                    let s = snap.screen[y * w + x];
                    let sc = [(s >> 16) as u8, (s >> 8) as u8, s as u8];
                    if (0..3).any(|i| (r[i] as i32 - sc[i] as i32).abs() > tol) {
                        n += 1;
                        bb = [bb[0].min(x as i32), bb[1].min(y as i32), bb[2].max(x as i32 + 1), bb[3].max(y as i32 + 1)];
                    }
                }
            }
            self.ctx.Unmap(&staging, 0);
            Some((n as f32 / (w * h) as f32, if n > 0 { bb } else { [0; 4] }))
        }
    }

    /// Saves the scene target (the full-resolution HD frame) as a PNG.
    pub fn save_scene(&self, path: &Path) -> Result<(), String> {
        let Some((tex, _, _, w, h)) = self.scene.clone() else { return Err("no scene yet".into()) };
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging)).map_err(|e| e.to_string())?;
            let staging = staging.unwrap();
            self.ctx.CopyResource(&staging, &tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| e.to_string())?;
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w as usize * 4);
                rgb.extend(row.chunks(4).flat_map(|p| [p[0], p[1], p[2]]));
            }
            self.ctx.Unmap(&staging, 0);
            write_png(path, w, h, png::ColorType::Rgb, &rgb)
        }
    }
}

pub fn write_png(path: &Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(data).map_err(|e| e.to_string())
}

/// 0xFFFFFFFF inside the box of every visible sprite, 0 elsewhere (low-res, top-down).
fn sprite_mask(snap: &Snapshot) -> Vec<u32> {
    let (w, h) = (snap.width as i32, snap.height as i32);
    let mut mask = vec![0u32; (w * h) as usize];
    for s in snap.sprites.iter().filter(|s| s.flags & SPRITE_HIDDEN == 0) {
        let (x1, y1) = ((s.x1 - snap.origin.0).clamp(0, w), (s.y1 - snap.origin.1).clamp(0, h));
        let (x2, y2) = ((s.x2 - snap.origin.0).clamp(x1, w), (s.y2 - snap.origin.1).clamp(y1, h));
        for y in y1..y2 {
            mask[(y * w + x1) as usize..(y * w + x2) as usize].fill(u32::MAX);
        }
    }
    mask
}

/// Blend mode and vertex colour for an ink effect; None means "not supported, use the low-res pixels".
fn ink(effect: u32, param: u32) -> Option<(Blend, [f32; 4])> {
    match effect & EFFECT_MASK {
        BOP_COPY => Some((Blend::Premul, [1.0; 4])),
        // dest*coef + src*(128-coef), coef 0..128
        BOP_BLEND => {
            let k = (128.0 - param.min(128) as f32) / 128.0;
            Some((Blend::Premul, [k; 4]))
        }
        BOP_ADD => Some((Blend::Add, [1.0; 4])),
        BOP_SUB => Some((Blend::Sub, [1.0; 4])),
        _ => None,
    }
}

/// Corners (TL, TR, BL, BR) of a sprite in logical coordinates, scaled and rotated about its hot spot.
fn sprite_corners(s: &Sprite, w: f32, h: f32, hx: f32, hy: f32) -> [(f32, f32); 4] {
    let (sx, sy) = (if s.scale_x == 0.0 { 1.0 } else { s.scale_x }, if s.scale_y == 0.0 { 1.0 } else { s.scale_y });
    let a = (s.angle as f32).to_radians();
    let (sin, cos) = a.sin_cos();
    [(-hx, -hy), (w - hx, -hy), (-hx, h - hy), (w - hx, h - hy)].map(|(x, y)| {
        let (x, y) = (x * sx, y * sy);
        // Counter-clockwise on screen, with y pointing down.
        (s.x as f32 + x * cos + y * sin, s.y as f32 - x * sin + y * cos)
    })
}

/// Tiles covering a mosaic fill: the tile grid starts at the fill origin and is clipped to the
/// rectangle. Returns (x, y, w, h, uv) per visible piece, logical coordinates.
fn tile_rects(op: &BackOp, tw: f32, th: f32) -> Vec<(f32, f32, f32, f32, [f32; 4])> {
    let mut out = Vec::new();
    if tw <= 0.0 || th <= 0.0 || op.w <= 0 || op.h <= 0 {
        return out;
    }
    let (l, t, r, b) = (op.x as f32, op.y as f32, (op.x + op.w) as f32, (op.y + op.h) as f32);
    let (ox, oy) = (op.org_x as f32, op.org_y as f32);
    let mut ty = oy + ((t - oy) / th).floor() * th;
    while ty < b {
        let mut tx = ox + ((l - ox) / tw).floor() * tw;
        while tx < r {
            let (x1, y1, x2, y2) = (tx.max(l), ty.max(t), (tx + tw).min(r), (ty + th).min(b));
            if x2 > x1 && y2 > y1 {
                let uv = [(x1 - tx) / tw, (y1 - ty) / th, (x2 - tx) / tw, (y2 - ty) / th];
                out.push((x1, y1, x2 - x1, y2 - y1, uv));
            }
            tx += tw;
        }
        ty += th;
    }
    out
}

/// COLORREF (0x00BBGGRR) as a premultiplied colour, faded by the ink's vertex colour.
fn colorref(c: u32, ink: [f32; 4]) -> [f32; 4] {
    let ch = |shift: u32| ((c >> shift) & 0xFF) as f32 / 255.0;
    [ch(0) * ink[0], ch(8) * ink[1], ch(16) * ink[2], ink[3]]
}

/// Like push_quad but with a colour per corner and no texture.
fn push_quad_colors(verts: &mut Vec<Vertex>, c: [[f32; 2]; 4], col: [[f32; 4]; 4]) -> u32 {
    let first = verts.len() as u32;
    let v = |i: usize| Vertex { pos: c[i], uv: [0.0, 0.0], col: col[i] };
    verts.extend_from_slice(&[v(0), v(1), v(2), v(1), v(3), v(2)]);
    first
}

/// Appends two triangles; corners are TL, TR, BL, BR; uv is (u1, v1, u2, v2). Returns first vertex.
fn push_quad(verts: &mut Vec<Vertex>, c: [[f32; 2]; 4], uv: [f32; 4], col: [f32; 4]) -> u32 {
    let first = verts.len() as u32;
    let v = |p: [f32; 2], u: f32, w: f32| Vertex { pos: p, uv: [u, w], col };
    let (tl, tr, bl, br) = (v(c[0], uv[0], uv[1]), v(c[1], uv[2], uv[1]), v(c[2], uv[0], uv[3]), v(c[3], uv[2], uv[3]));
    verts.extend_from_slice(&[tl, tr, bl, tr, br, bl]);
    first
}
