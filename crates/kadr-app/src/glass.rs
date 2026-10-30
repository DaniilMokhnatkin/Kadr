//! Plumbing shared by Kadr's own windows («Недавние», миниатюра, настройки): a Direct2D device,
//! a premultiplied composition swap chain over the window (so DWM materials show through
//! transparent pixels), and the small drawing vocabulary of the visual system.
use kadr_render::gpu::{Gpu, Shared};
use std::mem::ManuallyDrop;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Interface, Result};
use windows_numerics::Matrix3x2;

/// Factories and one GPU context; created on first use by each window.
pub struct Device {
    pub shared: Shared,
    pub gpu: Gpu,
}
impl Device {
    pub fn new(name: &str) -> Result<Self> {
        let shared = Shared::new()?;
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let adapter = unsafe { factory.EnumAdapters1(0)? };
        let gpu = Gpu::new(&shared, &adapter, name.into())?;
        Ok(Self { shared, gpu })
    }
}

pub struct Surface {
    pub swapchain: IDXGISwapChain1,
    pub target: Option<ID2D1Bitmap1>,
    pub size: (u32, u32),
    _visual: IDCompositionVisual,
    _composition: IDCompositionTarget,
}

pub fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
}

pub fn target_props(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: ManuallyDrop::new(None),
    }
}

/// Creates the composition swap chain for `hwnd` or resizes it to the client area.
pub fn ensure(slot: &mut Option<Surface>, device: &Device, hwnd: HWND) -> Result<()> {
    let gpu = &device.gpu;
    let size = client_size(hwnd);
    let props = target_props(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
    match slot {
        Some(s) if s.size == size && s.target.is_some() => {}
        Some(s) => unsafe {
            gpu.d2d.SetTarget(None);
            s.target = None;
            s.swapchain.ResizeBuffers(0, size.0, size.1, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            let surface: IDXGISurface = s.swapchain.GetBuffer(0)?;
            s.target = Some(gpu.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&raw const props))?);
            s.size = size;
        },
        None => unsafe {
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0,
                Height: size.1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                // Transparent pixels let the window material (or the desktop) through.
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain = gpu.factory.CreateSwapChainForComposition(&gpu.device, &desc, None::<&IDXGIOutput>)?;
            let surface: IDXGISurface = swapchain.GetBuffer(0)?;
            let target = gpu.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&raw const props))?;
            let composition = gpu.dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = gpu.dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            composition.SetRoot(&visual)?;
            gpu.dcomp.Commit()?;
            *slot = Some(Surface { swapchain, target: Some(target), size, _visual: visual, _composition: composition });
        },
    }
    Ok(())
}

/// Draws one frame with `paint` and presents it on the next vertical blank.
pub fn frame(device: &Device, surface: &Surface, paint: impl FnOnce() -> bool) -> bool {
    let ctx = &device.gpu.d2d;
    unsafe {
        if let Some(target) = &surface.target {
            ctx.SetTarget(target);
        }
        ctx.BeginDraw();
        ctx.SetTransform(&Matrix3x2::identity());
        ctx.Clear(Some(&rgb(0x000000, 0.0)));
        let moving = paint();
        ctx.SetTransform(&Matrix3x2::identity());
        let _ = ctx.EndDraw(None, None);
        let _ = surface.swapchain.Present(1, DXGI_PRESENT(0));
        moving
    }
}

/// Offscreen render for design snapshots; returns BGRA pixels.
pub fn render_offscreen(device: &Device, w: u32, h: u32, paint: impl FnOnce()) -> Result<kadr_doc::Image> {
    unsafe {
        let ctx = &device.gpu.d2d;
        let size = D2D_SIZE_U { width: w, height: h };
        let target = ctx.CreateBitmap(size, None, 0, &target_props(D2D1_BITMAP_OPTIONS_TARGET))?;
        ctx.SetTarget(&target);
        ctx.BeginDraw();
        ctx.SetTransform(&Matrix3x2::identity());
        ctx.Clear(Some(&rgb(0x000000, 0.0)));
        paint();
        ctx.EndDraw(None, None)?;
        let readback = ctx.CreateBitmap(
            size,
            None,
            0,
            &target_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
        )?;
        readback.CopyFromBitmap(None, &target, None)?;
        let mapped = readback.Map(D2D1_MAP_OPTIONS_READ)?;
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            bgra.extend_from_slice(std::slice::from_raw_parts(
                mapped.bits.add((y * mapped.pitch) as usize),
                (w * 4) as usize,
            ));
        }
        readback.Unmap()?;
        Ok(kadr_doc::Image::new(kadr_doc::Rect::new(0, 0, w as i32, h as i32), bgra))
    }
}

pub fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}
pub fn inside(r: D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
pub fn inflate(r: D2D_RECT_F, by: f32) -> D2D_RECT_F {
    rect(r.left - by, r.top - by, r.right + by, r.bottom + by)
}
pub fn fill(gpu: &Gpu, c: D2D1_COLOR_F, r: D2D_RECT_F, radius: f32) {
    unsafe {
        gpu.brush.SetColor(&c);
        gpu.d2d.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &gpu.brush);
    }
}
pub fn stroke(gpu: &Gpu, c: D2D1_COLOR_F, r: D2D_RECT_F, radius: f32, width: f32) {
    unsafe {
        gpu.brush.SetColor(&c);
        gpu.d2d.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius },
            &gpu.brush,
            width,
            None::<&ID2D1StrokeStyle>,
        );
    }
}
pub fn text(gpu: &Gpu, format: &IDWriteTextFormat, value: &str, r: D2D_RECT_F, c: D2D1_COLOR_F) {
    let utf16: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        gpu.brush.SetColor(&c);
        gpu.d2d.DrawText(&utf16, format, &r, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }
}
pub fn fade(mut c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    c.a *= a.clamp(0.0, 1.0);
    c
}
pub fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    let t = t.clamp(0.0, 1.0);
    D2D1_COLOR_F {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}
pub const fn rgb(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}
/// Soft shadow from stacked rounded rings, as in the overlay toolbar.
pub fn shadow(gpu: &Gpu, r: D2D_RECT_F, radius: f32, s: f32, spread: f32, drop: f32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let steps = 8;
    for i in 0..steps {
        let t = (i + 1) as f32 / steps as f32;
        let grow = spread * s * t;
        let dy = drop * s * t;
        let ring = rect(r.left - grow, r.top - grow + dy, r.right + grow, r.bottom + grow + dy);
        fill(gpu, rgb(0x000000, alpha / steps as f32 * (1.0 - t * 0.5)), ring, radius + grow);
    }
}
/// Scale about the centre of `r`, then shift by (`dx`, `dy`).
pub fn scaled_about(r: D2D_RECT_F, k: f32, dx: f32, dy: f32) -> Matrix3x2 {
    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: cx * (1.0 - k) + dx, M32: cy * (1.0 - k) + dy }
}
/// Draws `bitmap` to cover `r`, clipped to its rounded corners, at `opacity`.
pub fn rounded_image(
    device: &Device,
    bitmap: &ID2D1Bitmap1,
    image: (f32, f32),
    r: D2D_RECT_F,
    radius: f32,
    opacity: f32,
    cover: bool,
) {
    let ctx = &device.gpu.d2d;
    let rounded = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
    let geometry = unsafe { device.shared.d2d.CreateRoundedRectangleGeometry(&rounded) }
        .ok()
        .and_then(|g| g.cast::<ID2D1Geometry>().ok());
    let mut params = D2D1_LAYER_PARAMETERS1 {
        contentBounds: r,
        geometricMask: ManuallyDrop::new(geometry),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity,
        opacityBrush: ManuallyDrop::new(None),
        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
    };
    let (w, h) = image;
    let (tw, th) = (r.right - r.left, r.bottom - r.top);
    let k = if cover { (tw / w).max(th / h) } else { (tw / w).min(th / h) };
    let (dw, dh) = (w * k, h * k);
    let dest =
        rect(r.left + (tw - dw) / 2.0, r.top + (th - dh) / 2.0, r.left + (tw + dw) / 2.0, r.top + (th + dh) / 2.0);
    unsafe {
        ctx.PushLayer(&params, None::<&ID2D1Layer>);
        fill(&device.gpu, kadr_render::theme::Theme::current().tile, r, 0.0);
        ctx.DrawBitmap(bitmap, Some(&raw const dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        ctx.PopLayer();
        ManuallyDrop::drop(&mut params.geometricMask);
        ManuallyDrop::drop(&mut params.opacityBrush);
    }
}

/// Error text that stays readable on either appearance.
pub fn error_ink(theme: &kadr_render::theme::Theme) -> D2D1_COLOR_F {
    if theme.dark { rgb(0xff6961, 1.0) } else { rgb(0xc4161c, 1.0) }
}

/// Dark windows get the dark acrylic/Mica from DWM; called on every show, so a theme switch
/// while the app runs is picked up the next time the window opens.
pub fn match_appearance(hwnd: HWND) {
    let dark = windows::core::BOOL::from(kadr_render::theme::Theme::current().dark);
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            size_of_val(&dark) as u32,
        );
    }
}
