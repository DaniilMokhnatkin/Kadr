//! Захват через DXGI Desktop Duplication: свой дубликатор на каждый монитор.
//!
//! Режимы (сравниваем в M0):
//! - on-demand — дубликатор создаётся на каждый захват и сразу освобождается;
//! - прогретый — дубликатор живёт постоянно. Новый кадр есть, только если экран менялся,
//!   иначе наша копия с прошлого раза и так актуальна.

mod windows_list;

pub use windows_list::{foreground_window, top_level_windows};

use windows::Win32::Foundation::{E_FAIL, RECT};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication,
    IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, HMONITOR, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::core::{Error, Interface, Result};

use kadr_doc::{Image, Rect};
use kadr_win::{from_wide, perf};

pub struct Output {
    pub gpu: usize,
    pub name: String,
    /// Физические пиксели виртуального рабочего стола.
    pub rect: RECT,
    pub monitor: HMONITOR,
    pub rotation: DXGI_MODE_ROTATION,
    output: IDXGIOutput1,
    dup: Option<IDXGIOutputDuplication>,
    /// Наша копия последнего кадра на GPU — её и рисует оверлей.
    pub frame: Option<ID3D11Texture2D>,
    normalized: Option<Image>,
    /// The duplicator gives this monitor no picture here: it is captured through GDI.
    gdi: bool,
}

/// Адаптер, к которому подключены мониторы. GPU-контекст для него создаёт вызывающий.
pub struct Adapter {
    pub adapter: IDXGIAdapter1,
    pub name: String,
}

/// Все адаптеры с подключёнными мониторами и сами мониторы. `Output::gpu` — индекс в списке адаптеров.
pub fn enumerate() -> Result<(Vec<Adapter>, Vec<Output>)> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut adapters = Vec::new();
    let mut outputs = Vec::new();
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let desc = unsafe { adapter.GetDesc1()? };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let mut attached = Vec::new();
        for j in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(j) }) else { break };
            let d = unsafe { output.GetDesc()? };
            if d.AttachedToDesktop.as_bool() {
                attached.push((output, d));
            }
        }
        if attached.is_empty() {
            continue;
        }
        let gpu = adapters.len();
        adapters.push(Adapter { name: from_wide(&desc.Description), adapter });
        for (output, d) in attached {
            outputs.push(Output {
                gpu,
                name: from_wide(&d.DeviceName),
                rect: d.DesktopCoordinates,
                monitor: d.Monitor,
                rotation: d.Rotation,
                output: output.cast()?,
                dup: None,
                frame: None,
                normalized: None,
                gdi: false,
            });
        }
    }
    Ok((adapters, outputs))
}

impl Output {
    pub fn width(&self) -> i32 {
        self.rect.right - self.rect.left
    }

    pub fn height(&self) -> i32 {
        self.rect.bottom - self.rect.top
    }

    /// Снимает текущий кадр в `self.frame`. Возвращает, сколько мс ушло на создание дубликатора.
    pub fn capture(&mut self, device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<f64> {
        if self.gdi {
            self.capture_gdi(device)?;
            return Ok(0.0);
        }
        let mut dup_ms = 0.0;
        for attempt in 0..2 {
            let fresh = self.dup.is_none();
            if fresh {
                let t = perf::now();
                self.dup = Some(self.duplicate(device)?);
                dup_ms += perf::ms(t, perf::now());
            }
            // Свежий дубликатор сразу отдаёт полный кадр — ждём его. Прогретый без изменений на
            // экране кадра не даст, и тогда актуальна наша прошлая копия.
            let can_reuse = !fresh && self.frame.is_some();
            let mut timeout = if can_reuse { 0 } else { 500 };
            let mut waited = 0;
            loop {
                let dup = self.dup.as_ref().unwrap();
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                match unsafe { dup.AcquireNextFrame(timeout, &mut info, &mut resource) } {
                    // Некоторые драйверы первым кадром отдают только курсор, без картинки
                    // рабочего стола (её ещё нет — получится чёрный экран): ждём настоящую.
                    Ok(()) if info.LastPresentTime == 0 && !can_reuse => {
                        unsafe {
                            let _ = dup.ReleaseFrame();
                        }
                        if waited == 4 {
                            return self.fallback(device, "дубликатор не дал картинку рабочего стола").map(|()| dup_ms);
                        }
                        waited += 1;
                        timeout = 100;
                    }
                    Ok(()) => {
                        let copied = self.copy_frame(device, context, resource);
                        unsafe {
                            let _ = self.dup.as_ref().unwrap().ReleaseFrame();
                        }
                        copied?;
                        if !can_reuse && self.blank(device, context) {
                            return self.fallback(device, "дубликатор отдал чёрный кадр").map(|()| dup_ms);
                        }
                        return Ok(dup_ms);
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT && can_reuse => return Ok(dup_ms),
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                        return self.fallback(device, "дубликатор не прислал кадр").map(|()| dup_ms);
                    }
                    // Смена режима, secure desktop, полноэкранное приложение: пересоздаём один раз.
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST && attempt == 0 => {
                        self.dup = None;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Err(Error::from(DXGI_ERROR_ACCESS_LOST))
    }

    /// The duplicator gave no picture: this monitor through GDI instead. If GDI sees one, the
    /// duplicator is what fails here (some drivers and hybrid graphics), and the monitor stays on
    /// GDI until Kadr restarts; a picture black either way is just a black screen.
    fn fallback(&mut self, device: &ID3D11Device, why: &str) -> Result<()> {
        self.capture_gdi(device)?;
        if self.normalized.as_ref().is_some_and(|image| !blank_pixels(&image.bgra)) {
            kadr_win::log_warn!("захват {}: {why}; снимаю этот монитор через GDI", self.name);
            self.gdi = true;
            self.dup = None;
        }
        Ok(())
    }

    /// Whether the frame just copied is black all over, looked at in nine small patches.
    fn blank(&self, device: &ID3D11Device, context: &ID3D11DeviceContext) -> bool {
        if let Some(image) = &self.normalized {
            return blank_pixels(&image.bgra);
        }
        let Some(frame) = &self.frame else { return true };
        const TILE: u32 = 8;
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            frame.GetDesc(&mut desc);
            if desc.Width < TILE * 3 || desc.Height < TILE * 3 {
                return false;
            }
            let (w, h) = (desc.Width, desc.Height);
            desc.Width = TILE * 3;
            desc.Height = TILE * 3;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            if device.CreateTexture2D(&desc, None, Some(&mut staging)).is_err() {
                return false;
            }
            let Some(staging) = staging else { return false };
            for gy in 0..3 {
                for gx in 0..3 {
                    let (x, y) = ((w - TILE) * (gx * 2 + 1) / 6, (h - TILE) * (gy * 2 + 1) / 6);
                    let source = D3D11_BOX { left: x, top: y, front: 0, right: x + TILE, bottom: y + TILE, back: 1 };
                    context.CopySubresourceRegion(&staging, 0, gx * TILE, gy * TILE, 0, frame, 0, Some(&source));
                }
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            if context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).is_err() {
                return false;
            }
            let mut pixels = Vec::with_capacity((TILE * TILE * 9 * 4) as usize);
            for row in 0..TILE * 3 {
                let line = (mapped.pData as *const u8).add((row * mapped.RowPitch) as usize);
                pixels.extend_from_slice(std::slice::from_raw_parts(line, (TILE * 3 * 4) as usize));
            }
            context.Unmap(&staging, 0);
            blank_pixels(&pixels)
        }
    }

    /// The monitor's pixels the old way, through the screen's device context: slower than the
    /// duplicator, but every driver supports it.
    fn capture_gdi(&mut self, device: &ID3D11Device) -> Result<()> {
        let (w, h) = (self.width(), self.height());
        let mut bgra = vec![0u8; (w.max(0) * h.max(0) * 4) as usize];
        unsafe {
            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let copied = bitmap.as_ref().ok().map(|bitmap| {
                let old = SelectObject(memory, (*bitmap).into());
                let done =
                    BitBlt(memory, 0, 0, w, h, Some(screen), self.rect.left, self.rect.top, SRCCOPY | CAPTUREBLT);
                if done.is_ok() && !bits.is_null() {
                    std::ptr::copy_nonoverlapping(bits as *const u8, bgra.as_mut_ptr(), bgra.len());
                }
                SelectObject(memory, old);
                done
            });
            if let Ok(bitmap) = bitmap {
                let _ = DeleteObject(bitmap.into());
            }
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            match copied {
                Some(Ok(())) => {}
                Some(Err(e)) => return Err(e),
                None => return Err(Error::from(E_FAIL)),
            }
        }
        for px in bgra.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        self.upload(device, Image::new(self.bounds(), bgra))
    }

    /// `image` (already upright) as the frame on the GPU, and kept for reading back.
    fn upload(&mut self, device: &ID3D11Device, image: Image) -> Result<()> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: image.width(),
            Height: image.height(),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: image.bgra.as_ptr().cast(),
            SysMemPitch: image.width() * 4,
            SysMemSlicePitch: 0,
        };
        let mut frame = None;
        unsafe {
            device.CreateTexture2D(&desc, Some(&data), Some(&mut frame))?;
        }
        self.frame = frame;
        self.normalized = Some(image);
        Ok(())
    }

    pub fn release_duplication(&mut self) {
        self.dup = None;
    }

    pub fn release_frame(&mut self) {
        self.frame = None;
        self.normalized = None;
    }

    /// Копия последнего кадра в память (BGRA): для лупы, экспорта и черновиков. Вызывается уже после
    /// показа оверлея — горячий путь её не ждёт.
    pub fn read_pixels(&self, device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Image> {
        let frame = self.frame.as_ref().ok_or_else(|| Error::from(E_FAIL))?;
        if let Some(image) = &self.normalized {
            return Ok(image.clone());
        }
        read_texture(device, context, frame, self.rect.left, self.rect.top)
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(self.rect.left, self.rect.top, self.rect.right, self.rect.bottom)
    }

    fn duplicate(&self, device: &ID3D11Device) -> Result<IDXGIOutputDuplication> {
        // DuplicateOutput1 (Win10 1703+) сам приводит HDR-вывод к 8-битному BGRA.
        if let Ok(output5) = self.output.cast::<IDXGIOutput5>()
            && let Ok(dup) = unsafe { output5.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) }
        {
            return Ok(dup);
        }
        unsafe { self.output.DuplicateOutput(device) }
    }

    fn copy_frame(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        resource: Option<IDXGIResource>,
    ) -> Result<()> {
        let source: ID3D11Texture2D = resource.ok_or_else(|| Error::from(E_FAIL))?.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        if (2..=4).contains(&self.rotation.0) {
            let raw = read_texture(device, context, &source, self.rect.left, self.rect.top)?;
            let image = raw.rotated((self.rotation.0 - 1) as u8);
            if image.rect != self.bounds() {
                return Err(Error::from(E_FAIL));
            }
            return self.upload(device, image);
        }
        self.normalized = None;
        let fits = self.frame.as_ref().is_some_and(|frame| {
            let mut own = D3D11_TEXTURE2D_DESC::default();
            unsafe { frame.GetDesc(&mut own) };
            own.Width == desc.Width && own.Height == desc.Height && own.Format == desc.Format
        });
        if !fits {
            desc.Usage = D3D11_USAGE_DEFAULT;
            desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            desc.CPUAccessFlags = 0;
            desc.MiscFlags = 0;
            desc.MipLevels = 1;
            desc.ArraySize = 1;
            let mut texture = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
            self.frame = texture;
        }
        let frame = self.frame.as_ref().ok_or_else(|| Error::from(E_FAIL))?;
        unsafe { context.CopyResource(frame, &source) };
        Ok(())
    }
}

/// Black all over: no pixel brighter than the faintest dark grey.
fn blank_pixels(bgra: &[u8]) -> bool {
    bgra.as_chunks::<4>().0.iter().all(|p| p[0] <= 2 && p[1] <= 2 && p[2] <= 2)
}

fn read_texture(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    frame: &ID3D11Texture2D,
    left: i32,
    top: i32,
) -> Result<Image> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        frame.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.ok_or_else(|| Error::from(E_FAIL))?;
        context.CopyResource(&staging, frame);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (width, height) = (desc.Width as usize, desc.Height as usize);
        let mut bgra = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            let row = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
            bgra.extend_from_slice(std::slice::from_raw_parts(row, width * 4));
        }
        context.Unmap(&staging, 0);

        Ok(Image::new(Rect::new(left, top, left + width as i32, top + height as i32), bgra))
    }
}

#[cfg(test)]
mod blank_tests {
    use super::*;
    use kadr_render::gpu::{Gpu, Shared};
    fn texture(gpu: &Gpu, w: u32, h: u32, value: u8) -> ID3D11Texture2D {
        let pixels: Vec<u8> = (0..w * h).flat_map(|_| [value, value, value, 255]).collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        let mut t = None;
        unsafe { gpu.device.CreateTexture2D(&desc, Some(&data), Some(&mut t)).unwrap() };
        t.unwrap()
    }
    /// The black screen on some PCs: the duplicator «succeeds» with an empty picture. Kadr sees
    /// it, takes the monitor through GDI and stays on GDI; a dark but real picture is kept.
    #[test]
    fn a_black_frame_from_the_duplicator_is_replaced_by_the_gdi_picture() {
        assert!(blank_pixels(&[0, 0, 0, 255, 2, 1, 0, 0]));
        assert!(!blank_pixels(&[0, 0, 0, 255, 30, 30, 30, 255]));
        let shared = Shared::new().unwrap();
        let (adapters, mut outputs) = enumerate().unwrap();
        let mut output = outputs.remove(0);
        let gpu = Gpu::new(&shared, &adapters[output.gpu].adapter, "blank-test".into()).unwrap();
        output.rotation = DXGI_MODE_ROTATION(1);
        let (w, h) = (output.width() as u32, output.height() as u32);
        output.copy_frame(&gpu.device, &gpu.context, Some(texture(&gpu, w, h, 0).cast().unwrap())).unwrap();
        assert!(output.blank(&gpu.device, &gpu.context), "a black frame is blank");
        output.copy_frame(&gpu.device, &gpu.context, Some(texture(&gpu, w, h, 24).cast().unwrap())).unwrap();
        assert!(!output.blank(&gpu.device, &gpu.context), "a dark grey frame is a picture");
        output.fallback(&gpu.device, "test").unwrap();
        let image = output.read_pixels(&gpu.device, &gpu.context).unwrap();
        assert_eq!(image.rect, output.bounds());
        assert!(output.gdi && !blank_pixels(&image.bgra), "the desktop through GDI is a picture");
        // From now on the monitor is taken through GDI, with no duplicator involved.
        output.capture(&gpu.device, &gpu.context).unwrap();
        assert!(output.dup.is_none());
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use kadr_render::gpu::{Gpu, Shared};
    #[test]
    fn rotated_gpu_texture_and_export_share_the_same_upright_desktop_coordinates() {
        let shared = Shared::new().unwrap();
        let (adapters, mut outputs) = enumerate().unwrap();
        let gpu = Gpu::new(&shared, &adapters[0].adapter, "rotation-test".into()).unwrap();
        let mut output = outputs.remove(0);
        let pixels: Vec<_> = (1u8..=6).flat_map(|n| [n, 0, 0, 255]).collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 3,
            Height: 2,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: 12, SysMemSlicePitch: 0 };
        let mut texture = None;
        unsafe {
            gpu.device.CreateTexture2D(&desc, Some(&data), Some(&mut texture)).unwrap();
        }
        let texture: ID3D11Texture2D = texture.unwrap();
        for q in 1..=3 {
            let (w, h) = if q % 2 == 1 { (2, 3) } else { (3, 2) };
            output.rect = RECT { left: -w, top: 20, right: 0, bottom: 20 + h };
            output.rotation = DXGI_MODE_ROTATION(q + 1);
            output.copy_frame(&gpu.device, &gpu.context, Some(texture.cast().unwrap())).unwrap();
            let cpu = output.read_pixels(&gpu.device, &gpu.context).unwrap();
            let actual_gpu = read_texture(&gpu.device, &gpu.context, output.frame.as_ref().unwrap(), -w, 20).unwrap();
            let expected = Image::new(Rect::new(-w, 20, -w + 3, 22), pixels.clone()).rotated(q as u8);
            assert_eq!(cpu, expected);
            assert_eq!(actual_gpu, expected);
            assert_eq!(cpu.rect, output.bounds());
        }
    }
}
