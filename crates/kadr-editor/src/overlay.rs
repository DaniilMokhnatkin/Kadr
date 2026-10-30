//! Окно оверлея: одно на монитор. Создаётся при старте и прячется DWM-cloaking'ом,
//! поэтому показ на горячем пути — это только снятие маскировки с уже отрисованного окна.

use kadr_render::annotations::Cache;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE, HWND, POINT, RECT, WAIT_OBJECT_0};
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
    DXGI_ERROR_WAS_STILL_DRAWING, DXGI_PRESENT, DXGI_PRESENT_DO_NOT_WAIT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIOutput, IDXGISurface, IDXGISwapChain1, IDXGISwapChain2,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Threading::WaitForSingleObject;
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
    annotation_cache: RefCell<Cache>,
    latency: Option<HANDLE>,
    retry_present: Cell<bool>,
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
            } else {
                WS_EX_TOOLWINDOW
            };
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

            let mut desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width as u32,
                Height: height as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                // Desktop composition, not a fullscreen game's discard presentation path.
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                ..Default::default()
            };
            let swapchain =
                gpu.factory.CreateSwapChainForComposition(&gpu.device, &desc, None::<&IDXGIOutput>).or_else(|_| {
                    desc.Flags = 0;
                    gpu.factory.CreateSwapChainForComposition(&gpu.device, &desc, None::<&IDXGIOutput>)
                })?;
            let latency = if desc.Flags != 0 {
                let chain: IDXGISwapChain2 = swapchain.cast()?;
                chain.SetMaximumFrameLatency(1)?;
                let handle = chain.GetFrameLatencyWaitableObject();
                (!handle.is_invalid()).then_some(handle)
            } else {
                None
            };
            let surface: IDXGISurface = swapchain.GetBuffer(0)?;
            let props = bitmap_props(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
            let target = gpu.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&raw const props))?;

            let composition = gpu.dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = gpu.dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            composition.SetRoot(&visual)?;
            gpu.dcomp.Commit()?;

            let scale = monitor_scale(monitor);
            let text = TextStyles::new(shared, scale)?;
            text.enable_motion(kadr_render::theme::motion_enabled());
            Ok(Self {
                hwnd,
                output,
                rect: r,
                scale,
                text,
                frame: None,
                swapchain,
                target,
                _visual: visual,
                _composition: composition,
                annotation_cache: RefCell::new(Cache::default()),
                latency,
                retry_present: Cell::new(false),
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
        self.text.reset_motion();
        self.frame = None;
        *self.annotation_cache.borrow_mut() = Cache::default();
    }

    /// Загруженный черновик уже находится в памяти CPU, захват живого экрана не нужен.
    pub fn set_image(&mut self, gpu: &Gpu, image: &kadr_doc::Image) -> Result<()> {
        let size =
            windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U { width: image.width(), height: image.height() };
        self.frame = Some(unsafe {
            gpu.d2d.CreateBitmap(
                size,
                Some(image.bgra.as_ptr() as *const c_void),
                image.width() * 4,
                &bitmap_props(D2D1_BITMAP_OPTIONS_NONE),
            )?
        });
        Ok(())
    }

    pub fn prepare_annotations(
        &self,
        gpu: &Gpu,
        shared: &Shared,
        frames: &[std::sync::Arc<kadr_doc::Image>],
        crop: kadr_doc::Rect,
        shapes: &[kadr_doc::annotation::Annotation],
    ) -> Result<()> {
        kadr_render::annotations::prepare_display(
            gpu,
            shared,
            frames,
            crop,
            shapes,
            &mut self.annotation_cache.borrow_mut(),
        )
    }

    pub fn present(&self, gpu: &Gpu, shared: &Shared, theme: &Theme, marks: Marks) -> Result<()> {
        if !self.retry_present.get()
            && let Some(handle) = self.latency
        {
            unsafe {
                let _ = WaitForSingleObject(handle, 100);
            }
        }
        self.paint(gpu, shared, theme, marks, false).map(|_| ())
    }
    pub fn latency_handle(&self) -> Option<HANDLE> {
        self.latency
    }
    pub fn ready(&self) -> bool {
        self.retry_present.get() || self.latency.is_none_or(|h| unsafe { WaitForSingleObject(h, 0) == WAIT_OBJECT_0 })
    }
    pub fn needs_retry(&self) -> bool {
        self.retry_present.get()
    }
    pub fn animations_pending(&self) -> bool {
        self.text.animations_pending()
    }
    pub fn update_motion_preference(&self) {
        self.text.enable_motion(kadr_render::theme::motion_enabled());
    }
    pub fn try_present(&self, gpu: &Gpu, shared: &Shared, theme: &Theme, marks: Marks) -> Result<bool> {
        self.paint(gpu, shared, theme, marks, true)
    }
    fn paint(&self, gpu: &Gpu, shared: &Shared, theme: &Theme, marks: Marks, nonblocking: bool) -> Result<bool> {
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
            let drawn = scene::draw(gpu, shared, &self.text, theme, &scene, &mut self.annotation_cache.borrow_mut());
            gpu.d2d.EndDraw(None, None)?;
            drawn?;
            let result = self.swapchain.Present(
                if nonblocking { 1 } else { 0 },
                if nonblocking { DXGI_PRESENT_DO_NOT_WAIT } else { DXGI_PRESENT(0) },
            );
            present_result(result, nonblocking, &self.retry_present)
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
    pub fn toolbar_hit(
        &self,
        shared: &Shared,
        selection: RECT,
        p: POINT,
        editor: Option<kadr_render::scene::EditorBar>,
    ) -> Option<ToolbarButton> {
        scene::toolbar_hit(shared, &self.text, self.rect, self.scale, selection, (p.x, p.y), editor)
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
            if let Some(handle) = self.latency.take() {
                let _ = CloseHandle(handle);
            }
        }
    }
}

pub fn set_cloaked(hwnd: HWND, cloaked: bool) {
    set_bool_attribute(hwnd, DWMWA_CLOAK, cloaked);
}

fn set_bool_attribute(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: bool) {
    let value = BOOL::from(value);
    unsafe {
        let _ =
            DwmSetWindowAttribute(hwnd, attribute, &value as *const BOOL as *const c_void, size_of::<BOOL>() as u32);
    }
}

fn monitor_scale(monitor: HMONITOR) -> f32 {
    let (mut x, mut y) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
    }
    x as f32 / 96.0
}
fn present_result(result: windows::core::HRESULT, nonblocking: bool, retry: &Cell<bool>) -> Result<bool> {
    if nonblocking && result == DXGI_ERROR_WAS_STILL_DRAWING {
        retry.set(true);
        return Ok(false);
    }
    result.ok()?;
    retry.set(false);
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloaked_swapchain_keeps_advancing_without_any_desktop_input() {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, GetForegroundWindow, RegisterClassW, WNDCLASSW};
        unsafe extern "system" fn proc(
            hwnd: HWND,
            msg: u32,
            wp: windows::Win32::Foundation::WPARAM,
            lp: windows::Win32::Foundation::LPARAM,
        ) -> windows::Win32::Foundation::LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        let foreground = unsafe { GetForegroundWindow() };
        let shared = Shared::new().unwrap();
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
        let gpu =
            Gpu::new(&shared, &unsafe { factory.EnumAdapters1(0).unwrap() }, "idle animation test".into()).unwrap();
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap().into() };
        unsafe {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: CLASS,
                ..Default::default()
            });
        }
        // Always DWM-cloaked, offscreen, never activates; no capture, hooks or hotkeys.
        let overlay = Overlay::new(
            &gpu,
            &shared,
            0,
            RECT { left: -32000, top: -32000, right: -31360, bottom: -31520 },
            HMONITOR::default(),
            instance,
        )
        .unwrap();
        let handle = overlay.latency_handle().expect("DXGI frame signal");
        let mut frames = crate::pacing::Frames::default();
        frames.request(1);
        for i in 0..40 {
            assert_eq!(frames.pending(), vec![0]);
            assert_eq!(
                unsafe { WaitForSingleObject(handle, 500) },
                WAIT_OBJECT_0,
                "frame {i} must arrive without a mouse message"
            );
            let marks = Marks {
                selection: Some(RECT { left: -31950, top: -31950, right: -31450, bottom: -31600 }),
                ..Default::default()
            };
            let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while !overlay.try_present(&gpu, &shared, &Theme::current(), marks).unwrap() {
                assert!(std::time::Instant::now() < until, "present must make progress");
                std::thread::yield_now();
            }
            frames.presented(0, i != 39);
        }
        assert!(frames.pending().is_empty());
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    }
    #[test]
    fn busy_present_retains_readiness_until_success() {
        let retry = Cell::new(false);
        assert!(!present_result(DXGI_ERROR_WAS_STILL_DRAWING, true, &retry).unwrap());
        assert!(retry.get());
        assert!(present_result(windows::core::HRESULT(0), true, &retry).unwrap());
        assert!(!retry.get());
    }
}
