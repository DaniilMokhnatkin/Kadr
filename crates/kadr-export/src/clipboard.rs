//! Буфер обмена: DIB сразу (его понимают все), PNG — отложенно: кодируется, только когда его вставят.
//! Владелец буфера — скрытое окно приложения; оно передаёт сюда WM_RENDERFORMAT и WM_RENDERALLFORMATS
//! и держит картинку, пока не придёт WM_DESTROYCLIPBOARD.

use std::sync::OnceLock;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::core::{Error, Result, w};

use kadr_doc::Image;
use kadr_win::log_warn;

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;

/// Формат «PNG», который понимают браузеры, мессенджеры и Office: с ним сохраняется прозрачность.
pub fn png_format() -> u32 {
    static FORMAT: OnceLock<u32> = OnceLock::new();
    *FORMAT.get_or_init(|| unsafe { RegisterClipboardFormatW(w!("PNG")) })
}

/// Кладёт картинку в буфер. Саму картинку держит вызывающий — из неё по запросу кодируется PNG.
pub fn put_image(owner: HWND, image: &Image) -> Result<()> {
    with_clipboard(owner, || unsafe {
        set(CF_DIB, dib(image)?)?;
        // Отложенный рендеринг: данных пока нет, система попросит их через WM_RENDERFORMAT.
        let format = png_format();
        if let Err(error) = SetClipboardData(format, None) {
            // При hMem=NULL успешный результат тоже NULL. windows-rs считает его ошибкой;
            // проверяем, объявлен ли формат, пока буфер всё ещё принадлежит нам и открыт.
            IsClipboardFormatAvailable(format).map_err(|_| error)?;
        }
        Ok(())
    })
}

/// A file, as Explorer copies one (CF_HDROP): pasting into a chat or a folder hands over the file.
pub fn put_file(owner: HWND, path: &std::path::Path) -> Result<()> {
    put_files(owner, &[path.as_os_str().to_string_lossy().into_owned()])
}

/// Several files at once, as Explorer copies a selection.
pub fn put_files(owner: HWND, paths: &[String]) -> Result<()> {
    const CF_HDROP: u32 = 15;
    // DROPFILES: offset to the list, drop point, non-client flag, wide names; then the name list,
    // each name zero-terminated and the list ended by one more zero.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&20u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 12]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for name in paths {
        bytes.extend(name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    bytes.extend([0, 0]);
    with_clipboard(owner, || unsafe { set(CF_HDROP, global(&bytes)?) })
}

pub fn put_text(owner: HWND, text: &str) -> Result<()> {
    let bytes: Vec<u8> = text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
    with_clipboard(owner, || unsafe { set(CF_UNICODETEXT, global(&bytes)?) })
}
/// Ограниченный Unicode-текст для инструмента аннотаций. Буфер не очищается.
pub fn text(owner: HWND) -> Result<String> {
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::System::Memory::GlobalSize;
    unsafe {
        OpenClipboard(Some(owner))?;
        let result = (|| -> Result<String> {
            let handle = HGLOBAL(GetClipboardData(CF_UNICODETEXT)?.0);
            let size = GlobalSize(handle).min(32768) / 2;
            let ptr = GlobalLock(handle) as *const u16;
            if ptr.is_null() {
                return Err(Error::from_thread());
            }
            let data = std::slice::from_raw_parts(ptr, size);
            let end = data.iter().position(|c| *c == 0).unwrap_or(size);
            let text = String::from_utf16_lossy(&data[..end]);
            let _ = GlobalUnlock(handle);
            Ok(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

/// WM_RENDERFORMAT: буфер уже открыт системой, открывать его снова нельзя.
pub fn render(format: u32, image: &Image) {
    if format != png_format() {
        return;
    }
    match crate::encode_png(image) {
        Ok(bytes) => {
            if let Err(e) = unsafe { global(&bytes).and_then(|h| set(format, h)) } {
                log_warn!("PNG в буфер обмена: {e}");
            }
        }
        Err(e) => log_warn!("кодирование PNG для буфера: {e}"),
    }
}

/// WM_RENDERALLFORMATS — программа закрывается: отдать PNG, пока буфер ещё наш.
pub fn render_all(owner: HWND, image: &Image) {
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return;
        }
        if GetClipboardOwner().is_ok_and(|current| current == owner) {
            render(png_format(), image);
        }
        let _ = CloseClipboard();
    }
}

fn with_clipboard(owner: HWND, fill: impl FnOnce() -> Result<()>) -> Result<()> {
    unsafe {
        OpenClipboard(Some(owner))?;
        let result = EmptyClipboard().and_then(|()| fill());
        let _ = CloseClipboard();
        result
    }
}

/// После успешного SetClipboardData памятью владеет система; при ошибке освобождаем сами.
unsafe fn set(format: u32, memory: HGLOBAL) -> Result<()> {
    match unsafe { SetClipboardData(format, Some(HANDLE(memory.0))) } {
        Ok(_) => Ok(()),
        Err(e) => {
            unsafe {
                let _ = GlobalFree(Some(memory));
            }
            Err(e)
        }
    }
}

/// DIB: заголовок и пиксели BGRA снизу вверх, сразу в общую память — без лишней копии.
fn dib(image: &Image) -> Result<HGLOBAL> {
    let row = image.width() as usize * 4;
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: image.width() as i32,
        biHeight: image.height() as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: image.bgra.len() as u32,
        ..Default::default()
    };
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, size_of::<BITMAPINFOHEADER>() + image.bgra.len())?;
        let ptr = GlobalLock(memory) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(
            &header as *const BITMAPINFOHEADER as *const u8,
            ptr,
            size_of::<BITMAPINFOHEADER>(),
        );
        let mut at = ptr.add(size_of::<BITMAPINFOHEADER>());
        for line in image.bgra.chunks_exact(row).rev() {
            std::ptr::copy_nonoverlapping(line.as_ptr(), at, row);
            at = at.add(row);
        }
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }
}

unsafe fn global(bytes: &[u8]) -> Result<HGLOBAL> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let ptr = GlobalLock(memory) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP};

    /// Нужен обычный интерактивный рабочий стол Windows (не Low-integrity sandbox).
    #[test]
    fn copies_dib_and_advertises_delayed_png_without_reporting_failure() {
        let owner = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Kadr clipboard test"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap();
        let image = Image::new(kadr_doc::Rect::new(0, 0, 2, 2), vec![255; 16]);
        let result = put_image(owner, &image);
        let dib_available = unsafe { IsClipboardFormatAvailable(CF_DIB) }.is_ok();
        let png_available = unsafe { IsClipboardFormatAvailable(png_format()) }.is_ok();
        render_all(owner, &image);
        unsafe {
            OpenClipboard(Some(owner)).unwrap();
        }
        let png_rendered = unsafe { GetClipboardData(png_format()) }.is_ok();
        unsafe {
            CloseClipboard().unwrap();
        }
        unsafe {
            DestroyWindow(owner).unwrap();
        }
        assert!(result.is_ok(), "копирование завершилось ошибкой: {result:?}");
        assert!(dib_available && png_available && png_rendered);
    }
}
