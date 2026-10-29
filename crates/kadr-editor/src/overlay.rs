//! Окно оверлея: одно на монитор. Создаётся при старте и прячется DWM-cloaking'ом,
//! поэтому показ на горячем пути — это только снятие маскировки с уже отрисованного окна.

use std::ffi::c_void;

use windows::Win32::Foundation::{HINSTANCE, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, ID2D1Bitmap1,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWINDOWATTRIBUTE, DwmFlush, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIOutput, IDXGISurface, IDXGISwapChain1,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, ShowWindow,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, Interface, PCWSTR, Result, w};

use kadr_render::gpu::{Gpu, Shared, bitmap_props};
use kadr_render::scene::{self, Marks, Scene, TextStyles, ToolbarButton};
use kadr_render::theme::Theme;

pub const CLASS: PCWSTR = w!("KadrOverlay");

pub struct Overlay {
    pub hwnd: HWND,
    pub output: usize,
    pub rect: RECT,
    pub scale: f32,
    text: TextStyles,
    frame: Option<ID2D1Bitmap1>,
    swapchain: IDXGISwapChain1,
    target: ID2D1Bitmap1,
    _visual: IDCompositionVisual,
    _composition: IDCompositionTarget,
}

impl Overlay {
    /// `rect` — монитор в физических пикселях виртуального рабочего стола.
    pub fn new(
        gpu: &Gpu,
        shared: &Shared,
        output: usize,
        rect: RECT,
        monitor: HMONITOR,
        instance: HINSTANCE,
    ) -> Result<Self> {
        let r = rect;
        let (width, height) = (r.right - r.left, r.bottom - r.top);
        unsafe {
            // Только изолированный debug-сеанс: окна доступны нативным UI-тестам.
            let tool_style = if cfg!(debug_assertions) && std::env::var_os("KADR_DEV_INSTANCE").is_some() {
                Default::default()
            } else { WS_EX_TOOLWINDOW };
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | tool_style | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                w!("Kadr"),
                WS_POPUP,
                r.left,
                r.top,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            )?;
            set_cloaked(hwnd, true);
            set_bool_attribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, true);
            let _ = ShowWindow(hwnd, SW_SHOWNA);

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width as u32,
                Height: height as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swapchain = gpu.factory.CreateSwapChainForComposition(&gpu.device, &desc, None::<&IDXGIOutput>)?;
            let surface: IDXGISurface = swapchain.GetBuffer(0)?;
            let props = bitmap_props(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
            let target = gpu.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&raw const props))?;

            let composition = gpu.dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = gpu.dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            composition.SetRoot(&visual)?;
            gpu.dcomp.Commit()?;

            let scale = monitor_scale(monitor);
            Ok(Self {
                hwnd,
                output,
                rect: r,
                scale,
                text: TextStyles::new(shared, scale)?,
                frame: None,
                swapchain,
                target,
                _visual: visual,
                _composition: composition,
            })
        }
    }

    /// Привязывает кадр монитора (GPU-текстуру захвата) без копирования пикселей.
    pub fn set_frame(&mut self, gpu: &Gpu, texture: Option<&ID3D11Texture2D>) {
        self.frame = texture.and_then(|texture| {
            let surface: IDXGISurface = texture.cast().ok()?;
            let props = bitmap_props(D2D1_BITMAP_OPTIONS_NONE);
            unsafe { gpu.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&raw const props)) }.ok()
        });
    }

    pub fn clear_frame(&mut self) {
        self.frame = None;
    }

    /// Загруженный черновик уже находится в памяти CPU, захват живого экрана не нужен.
    pub fn set_image(&mut self, gpu: &Gpu, image: &kadr_doc::Image) -> Result<()> {
        let size = windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
            width: image.width(), height: image.height(),
        };
        self.frame = Some(unsafe { gpu.d2d.CreateBitmap(size,
            Some(image.bgra.as_ptr() as *const c_void), image.width()*4,
            &bitmap_props(D2D1_BITMAP_OPTIONS_NONE))? });
        Ok(())
    }

    pub fn present(&self, gpu: &Gpu, shared: &Shared, theme: &Theme, marks: Marks) -> Result<()> {
        let scene = Scene {
            frame: self.frame.as_ref(),
            origin: (self.rect.left, self.rect.top),
            size: ((self.rect.right - self.rect.left) as u32, (self.rect.bottom - self.rect.top) as u32),
            scale: self.scale,
            marks,
        };
        unsafe {
            gpu.d2d.SetTarget(&self.target);
            gpu.d2d.BeginDraw();
            let drawn = scene::draw(gpu, shared, &self.text, theme, &scene);
            gpu.d2d.EndDraw(None, None)?;
            drawn?;
            // Без ожидания vsync: DWM сам возьмёт самый свежий кадр (flip-discard).
            self.swapchain.Present(0, DXGI_PRESENT(0)).ok()
        }
    }

    /// Первый показ окна стоит ~20 мс (DWM заводит композицию). Делаем его при старте —
    /// за пределами экрана, чтобы ничего не мелькнуло.
    pub fn warm_up_composition(&self) {
        let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, -32000, -32000, 0, 0, flags);
            set_cloaked(self.hwnd, false);
            let _ = DwmFlush();
            set_cloaked(self.hwnd, true);
            let _ = SetWindowPos(self.hwnd, None, self.rect.left, self.rect.top, 0, 0, flags);
        }
    }

    /// Кнопка панели под точкой экрана, если панель нарисована на этом мониторе.
    pub fn toolbar_hit(&self, shared: &Shared, selection: RECT, p: POINT) -> Option<ToolbarButton> {
        scene::toolbar_hit(shared, &self.text, self.rect, self.scale, selection, (p.x, p.y))
    }

    pub fn contains(&self, p: POINT) -> bool {
        let r = self.rect;
        p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

pub fn set_cloaked(hwnd: HWND, cloaked: bool) {
    set_bool_attribute(hwnd, DWMWA_CLOAK, cloaked);
}

fn set_bool_attribute(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: bool) {
    let value = BOOL::from(value);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attribute,
            &value as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        );
    }
}

fn monitor_scale(monitor: HMONITOR) -> f32 {
    let (mut x, mut y) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
    }
    x as f32 / 96.0
}
