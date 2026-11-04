//! Direct3D 11 side of Skin Lab. The skin's pictures are drawn once into a stack the size of
//! Live's window; each captured frame of Live is then combined with it into either a
//! premultiplied-alpha overlay in a DirectComposition swap chain on our window, or (preview) an
//! opaque picture of what the screen will show, read back to the CPU.

use windows::core::{s, Error, Interface, Result, PCSTR};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

pub const MAX_PANELS: usize = 6;

/// Mirrors `cbuffer Params` in shader.hlsl.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Params {
    pub key: [f32; 4],
    pub misc: [f32; 4],
    pub place: [f32; 4],
    pub look: [f32; 4],
    pub ink: [f32; 4],
    pub pic: [f32; 4],
    pub flags: [f32; 4],
    pub panels: [[f32; 4]; MAX_PANELS],
    pub backdrop: [[f32; 4]; MAX_PANELS],
}

impl Params {
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts((self as *const Params).cast(), std::mem::size_of::<Params>()) }
    }
}

pub struct Pic {
    pub srv: ID3D11ShaderResourceView,
    pub w: u32,
    pub h: u32,
    pub mips: u32,
    /// lightness of the paper, for line art
    pub paper: f32,
}

/// One picture, ready to draw into the stack.
pub struct Draw {
    pub p: Params,
    pub srv: ID3D11ShaderResourceView,
    /// the panel it is cut to: left, top, right, bottom in frame pixels
    pub clip: [i32; 4],
}

struct Target {
    tex: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    srv: ID3D11ShaderResourceView,
}

struct Output {
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
    swap: Option<IDXGISwapChain1>,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
}

pub struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    pub winrt: IDirect3DDevice,
    factory: IDXGIFactory2,
    vs: ID3D11VertexShader,
    ps_layer: ID3D11PixelShader,
    ps_combine: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    over: ID3D11BlendState,
    scissor: ID3D11RasterizerState,
    out: Option<Output>,
    ui: Option<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    /// size of the current frame
    pub ui_size: (u32, u32),
    stack: Option<Target>,
    /// what the stack holds: a hash of its size and draws
    stack_key: u64,
}

fn compile(src: &str, entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let r = D3DCompile(
            src.as_ptr().cast(),
            src.len(),
            s!("skinlab.hlsl"),
            None,
            None::<&ID3DInclude>,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        );
        let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec();
        if let Err(e) = r {
            let msg = errors.map(|b| String::from_utf8_lossy(&bytes(&b)).into_owned()).unwrap_or_default();
            return Err(Error::new(e.code(), msg));
        }
        Ok(bytes(&code.expect("D3DCompile gave no code")))
    }
}

fn tex_desc(w: u32, h: u32, format: DXGI_FORMAT, usage: D3D11_USAGE, bind: D3D11_BIND_FLAG, cpu: D3D11_CPU_ACCESS_FLAG) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: cpu.0 as u32,
        MiscFlags: 0,
    }
}

impl Gpu {
    /// Device and shaders; with a window, also a DirectComposition target on it.
    pub fn new(hwnd: Option<HWND>) -> Result<Gpu> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
            let device: ID3D11Device = device.expect("no D3D11 device");
            let ctx = ctx.expect("no D3D11 context");
            let dxgi: IDXGIDevice = device.cast()?;
            if let Ok(d1) = dxgi.cast::<IDXGIDevice1>() {
                // show the newest frame, never queue old ones
                let _ = d1.SetMaximumFrameLatency(1);
            }
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;

            let src = include_str!("shader.hlsl");
            let vsb = compile(src, s!("vs_main"), s!("vs_5_0"))?;
            let mut vs = None;
            device.CreateVertexShader(&vsb, None, Some(&mut vs))?;
            let pixel = |entry: PCSTR| -> Result<ID3D11PixelShader> {
                let b = compile(src, entry, s!("ps_5_0"))?;
                let mut ps = None;
                device.CreatePixelShader(&b, None, Some(&mut ps))?;
                Ok(ps.expect("pixel shader"))
            };
            let ps_layer = pixel(s!("ps_layer"))?;
            let ps_combine = pixel(s!("ps_combine"))?;
            let mut cb = None;
            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Params>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&cb_desc, None, Some(&mut cb))?;
            let mut sampler = None;
            let s_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&s_desc, Some(&mut sampler))?;
            // premultiplied "over": each picture goes on top of the ones before it
            let mut b_desc = D3D11_BLEND_DESC::default();
            b_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut over = None;
            device.CreateBlendState(&b_desc, Some(&mut over))?;
            let r_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ScissorEnable: true.into(),
                ..Default::default()
            };
            let mut scissor = None;
            device.CreateRasterizerState(&r_desc, Some(&mut scissor))?;

            let out = match hwnd {
                Some(h) => {
                    let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
                    let target = dcomp.CreateTargetForHwnd(h, true)?;
                    let visual = dcomp.CreateVisual()?;
                    target.SetRoot(&visual)?;
                    dcomp.Commit()?;
                    Some(Output { dcomp, _target: target, visual, swap: None, rtv: None, size: (0, 0) })
                }
                None => None,
            };
            Ok(Gpu {
                device,
                ctx,
                winrt,
                factory,
                vs: vs.expect("vertex shader"),
                ps_layer,
                ps_combine,
                cb: cb.expect("constant buffer"),
                sampler: sampler.expect("sampler"),
                over: over.expect("blend state"),
                scissor: scissor.expect("rasterizer state"),
                out,
                ui: None,
                ui_size: (0, 0),
                stack: None,
                stack_key: 0,
            })
        }
    }

    fn ensure_ui(&mut self, w: u32, h: u32, data: Option<&[u8]>) -> Result<()> {
        if self.ui.is_some() && self.ui_size == (w, h) && data.is_none() {
            return Ok(());
        }
        unsafe {
            let desc = tex_desc(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_FLAG(0));
            let init = data.map(|d| D3D11_SUBRESOURCE_DATA { pSysMem: d.as_ptr().cast(), SysMemPitch: w * 4, SysMemSlicePitch: 0 });
            let mut t = None;
            self.device.CreateTexture2D(&desc, init.as_ref().map(|i| i as *const _), Some(&mut t))?;
            let t = t.expect("frame texture");
            let mut srv = None;
            self.device.CreateShaderResourceView(&t, None, Some(&mut srv))?;
            self.ui = Some((t, srv.expect("frame view")));
            self.ui_size = (w, h);
        }
        Ok(())
    }

    /// Copies a captured frame (the top-left `w`×`h` of `src`) into our own texture.
    /// False when `src` is smaller than that: the capture pool has not caught up with a resize.
    pub fn take_frame(&mut self, src: &ID3D11Texture2D, w: u32, h: u32) -> Result<bool> {
        unsafe {
            let mut sd = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut sd);
            if sd.Width < w || sd.Height < h || w == 0 || h == 0 {
                return Ok(false);
            }
            self.ensure_ui(w, h, None)?;
            let (dst, _) = self.ui.as_ref().expect("frame texture");
            let b = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
            self.ctx.CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, Some(&b));
            Ok(true)
        }
    }

    /// A frame from memory (BGRA rows), for previews.
    pub fn set_frame(&mut self, w: u32, h: u32, bgra: &[u8]) -> Result<()> {
        self.ensure_ui(w, h, Some(bgra))
    }

    pub fn forget_frame(&mut self) {
        self.ui = None;
        self.ui_size = (0, 0);
    }

    /// The current frame as BGRA rows.
    pub fn read_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (src, _) = self.ui.as_ref()?;
        let (w, h) = self.ui_size;
        self.read_texture(src, w, h).ok().map(|px| (w, h, px))
    }

    fn read_texture(&self, src: &ID3D11Texture2D, w: u32, h: u32) -> Result<Vec<u8>> {
        unsafe {
            let desc = tex_desc(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_USAGE_STAGING, D3D11_BIND_FLAG(0), D3D11_CPU_ACCESS_READ);
            let mut st = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut st))?;
            let st = st.expect("staging texture");
            self.ctx.CopyResource(&st, src);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let mut px = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), (w * 4) as usize);
                px.extend_from_slice(row);
            }
            self.ctx.Unmap(&st, 0);
            Ok(px)
        }
    }

    fn pass(&self, rtv: &ID3D11RenderTargetView, w: u32, h: u32, ps: &ID3D11PixelShader) {
        unsafe {
            let ctx = &self.ctx;
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None::<&ID3D11DepthStencilView>);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.RSSetState(&self.scissor);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.IASetInputLayout(None::<&ID3D11InputLayout>);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        }
    }

    /// Draws the pictures into the stack, unless it already holds exactly these.
    pub fn build_stack(&mut self, draws: &[Draw]) -> Result<()> {
        let (w, h) = self.ui_size;
        if w == 0 || h == 0 {
            return Ok(());
        }
        let mut bytes = [w.to_le_bytes(), h.to_le_bytes()].concat();
        for d in draws {
            bytes.extend_from_slice(d.p.bytes());
            bytes.extend_from_slice(&(d.srv.as_raw() as usize).to_le_bytes());
            d.clip.iter().for_each(|v| bytes.extend_from_slice(&v.to_le_bytes()));
        }
        let key = crate::config::fnv(&bytes);
        if self.stack.is_some() && key == self.stack_key {
            return Ok(());
        }
        unsafe {
            let ctx = &self.ctx;
            // the stack must not stay bound for reading while it is drawn into
            ctx.PSSetShaderResources(0, Some(&[None, None, None]));
            let size_ok = self.stack.as_ref().is_some_and(|t| {
                let mut d = D3D11_TEXTURE2D_DESC::default();
                t.tex.GetDesc(&mut d);
                (d.Width, d.Height) == (w, h)
            });
            if !size_ok {
                self.stack = None;
                let bind = D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0);
                let desc = tex_desc(w, h, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_USAGE_DEFAULT, bind, D3D11_CPU_ACCESS_FLAG(0));
                let mut t = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut t))?;
                let tex = t.expect("stack texture");
                let (mut rtv, mut srv) = (None, None);
                self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
                self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
                self.stack = Some(Target { tex, rtv: rtv.expect("stack target"), srv: srv.expect("stack view") });
            }
            let rtv = self.stack.as_ref().expect("stack").rtv.clone();
            ctx.ClearRenderTargetView(&rtv, &[0.0; 4]);
            self.pass(&rtv, w, h, &self.ps_layer);
            ctx.OMSetBlendState(&self.over, None, 0xffff_ffff);
            for d in draws {
                let [l, t, r, b] = d.clip;
                let clip = RECT { left: l.max(0), top: t.max(0), right: r.min(w as i32), bottom: b.min(h as i32) };
                if clip.right <= clip.left || clip.bottom <= clip.top {
                    continue;
                }
                ctx.RSSetScissorRects(Some(&[clip]));
                ctx.UpdateSubresource(&self.cb, 0, None, (&d.p as *const Params).cast(), 0, 0);
                ctx.PSSetShaderResources(2, Some(&[Some(d.srv.clone())]));
                ctx.Draw(3, 0);
            }
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.PSSetShaderResources(2, Some(&[None]));
        }
        self.stack_key = key;
        Ok(())
    }

    /// The current frame combined with the stack, into a `w`x`h` target.
    fn combine(&self, rtv: &ID3D11RenderTargetView, w: u32, h: u32, p: &Params) {
        let (Some((_, ui_srv)), Some(stack)) = (&self.ui, &self.stack) else { return };
        unsafe {
            self.pass(rtv, w, h, &self.ps_combine);
            self.ctx.RSSetScissorRects(Some(&[RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }]));
            self.ctx.UpdateSubresource(&self.cb, 0, None, (p as *const Params).cast(), 0, 0);
            self.ctx.PSSetShaderResources(0, Some(&[Some(ui_srv.clone()), Some(stack.srv.clone())]));
            self.ctx.Draw(3, 0);
            self.ctx.PSSetShaderResources(0, Some(&[None, None]));
        }
    }

    /// Draws the overlay for the current frame into the window and presents it. `p` carries
    /// the key colour.
    pub fn render(&mut self, draws: &[Draw], p: &Params) -> Result<()> {
        let (w, h) = self.ui_size;
        if self.ui.is_none() || self.out.is_none() {
            return Ok(());
        }
        self.build_stack(draws)?;
        let out = self.out.as_mut().expect("output");
        if out.swap.is_none() || out.size != (w, h) {
            unsafe {
                out.rtv = None;
                self.ctx.OMSetRenderTargets(None, None::<&ID3D11DepthStencilView>);
                self.ctx.Flush();
                match &out.swap {
                    Some(sc) => sc.ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))?,
                    None => {
                        let desc = DXGI_SWAP_CHAIN_DESC1 {
                            Width: w,
                            Height: h,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            Stereo: false.into(),
                            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                            BufferCount: 2,
                            Scaling: DXGI_SCALING_STRETCH,
                            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                            Flags: 0,
                        };
                        let sc = self.factory.CreateSwapChainForComposition(&self.device, &desc, None::<&IDXGIOutput>)?;
                        out.visual.SetContent(&sc)?;
                        out.dcomp.Commit()?;
                        out.swap = Some(sc);
                    }
                }
                let bb: ID3D11Texture2D = out.swap.as_ref().expect("swap chain").GetBuffer(0)?;
                let mut rtv = None;
                self.device.CreateRenderTargetView(&bb, None, Some(&mut rtv))?;
                out.rtv = rtv;
                out.size = (w, h);
            }
        }
        let (Some(rtv), Some(swap)) = (out.rtv.clone(), out.swap.clone()) else { return Ok(()) };
        let mut p = *p;
        p.misc[2] = 0.0;
        p.misc[3] = 1.0;
        self.combine(&rtv, w, h, &p);
        unsafe { swap.Present(1, DXGI_PRESENT(0)).ok() }
    }

    /// What the screen will show (overlay over the frame), `scale` times smaller, as RGBA rows.
    pub fn preview(&mut self, draws: &[Draw], p: &Params, scale: f32) -> Result<(u32, u32, Vec<u8>)> {
        self.build_stack(draws)?;
        let (fw, fh) = self.ui_size;
        let (w, h) = (((fw as f32 / scale).round() as u32).max(1), ((fh as f32 / scale).round() as u32).max(1));
        unsafe {
            let desc = tex_desc(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_FLAG(0));
            let mut t = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut t))?;
            let t = t.expect("preview texture");
            let mut rtv = None;
            self.device.CreateRenderTargetView(&t, None, Some(&mut rtv))?;
            let mut p = *p;
            p.misc[2] = 1.0;
            p.misc[3] = fw as f32 / w as f32;
            self.combine(&rtv.expect("preview view"), w, h, &p);
            let mut px = self.read_texture(&t, w, h)?;
            for c in px.chunks_exact_mut(4) {
                c.swap(0, 2);
            }
            Ok((w, h, px))
        }
    }

    /// Uploads a picture (premultiplied RGBA) with a full mip chain: the shader blurs by sampling
    /// smaller mips.
    pub fn load_picture(&self, img: &image::RgbaImage) -> Result<Pic> {
        let (w, h) = img.dimensions();
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 0,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
            };
            let mut t = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut t))?;
            let t = t.expect("picture texture");
            self.ctx.UpdateSubresource(&t, 0, None, img.as_ptr().cast(), w * 4, 0);
            let mut srv = None;
            self.device.CreateShaderResourceView(&t, None, Some(&mut srv))?;
            let srv = srv.expect("picture view");
            self.ctx.GenerateMips(&srv);
            Ok(Pic { srv, w, h, mips: 32 - w.max(h).leading_zeros(), paper: 1.0 })
        }
    }
}
