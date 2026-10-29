use super::*;
pub(super) fn footer(scale: f32) -> i32 {
    (64.0 * scale).round() as i32
}
fn color(c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    COLORREF((c.r * 255.0) as u32 | ((c.g * 255.0) as u32) << 8 | ((c.b * 255.0) as u32) << 16)
}
fn fill(dc: HDC, r: RECT, c: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(c);
        FillRect(dc, &r, brush);
        let _ = DeleteObject(brush.into());
    }
}
fn text(dc: HDC, mut r: RECT, value: &str, font: HFONT, c: COLORREF) {
    unsafe {
        let old = SelectObject(dc, font.into());
        SetTextColor(dc, c);
        SetBkMode(dc, TRANSPARENT);
        let mut chars: Vec<_> = value.encode_utf16().collect();
        DrawTextW(
            dc,
            &mut chars,
            &mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(dc, old);
    }
}
pub(super) fn label(id: usize, pin: &Pin) -> String {
    match id {
        COPY => "Копировать".into(),
        MINUS => "−".into(),
        PLUS => "+".into(),
        ORIGINAL => format!("{} %", (pin.zoom * 100.0).round() as i32),
        OPACITY => format!("Видимость {} %", (pin.opacity as f64 / 2.55).round() as i32),
        CLOSE => "Закрыть".into(),
        _ => String::new(),
    }
}
pub(super) fn accessible_label(id: usize, pin: &Pin) -> String {
    match id {
        COPY => "Копировать снимок, Ctrl+C".into(),
        MINUS => "Уменьшить масштаб".into(),
        PLUS => "Увеличить масштаб".into(),
        ORIGINAL => format!("Сбросить масштаб, сейчас {}", label(id, pin)),
        OPACITY => format!("Изменить прозрачность, {}", label(id, pin)),
        CLOSE => "Закрыть закреплённый снимок, Escape".into(),
        _ => label(id, pin),
    }
}
pub(super) fn buttons(r: RECT, scale: f32) -> Vec<RECT> {
    let gap = (8.0 * scale).round() as i32;
    let pad = (14.0 * scale).round() as i32;
    let top = r.bottom - footer(scale) + pad;
    let width = (r.right - 2 * pad - 5 * gap).max(6);
    let weights = [104, 36, 74, 36, 140, 80];
    let sum: i32 = weights.iter().sum();
    let mut left = pad;
    weights
        .iter()
        .enumerate()
        .map(|(i, weight)| {
            let right = if i == 5 {
                r.right - pad
            } else {
                left + width * weight / sum
            };
            let out = RECT {
                left,
                top,
                right,
                bottom: top + (36.0 * scale).round() as i32,
            };
            left = right + gap;
            out
        })
        .collect()
}
#[allow(clippy::too_many_arguments)]
pub(super) fn button(dc: HDC, r: RECT, id: usize, pin: &Pin, font: HFONT, focused: bool, pressed: bool, hovered: f32) {
    let t = Theme::current();
    fill(dc, r, color(t.surface));
    let fg = crate::chrome::button(
        dc,
        r,
        id == COPY,
        focused,
        pressed,
        hovered,
        (r.bottom - r.top) as f32 / 36.0,
    );
    let inset = RECT {
        left: r.left + 4,
        top: r.top + 1,
        right: r.right - 4,
        bottom: r.bottom - 1,
    };
    text(dc, inset, &label(id, pin), font, fg);
}
pub(super) fn draw(dc: HDC, r: RECT, image: &Image, pin: &Pin, scale: f32) {
    let t = Theme::current();
    fill(dc, r, color(t.surface));
    let canvas_h = (r.bottom - footer(scale)).max(1);
    let desired = pin.size();
    let factor = (r.right as f64 / desired.0 as f64)
        .min(canvas_h as f64 / desired.1 as f64)
        .min(1.0);
    let width = (desired.0 as f64 * factor).round() as i32;
    let height = (desired.1 as f64 * factor).round() as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: image.width() as i32,
            biHeight: -(image.height() as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let old = SetStretchBltMode(dc, HALFTONE);
        let _ = SetBrushOrgEx(dc, 0, 0, None);
        StretchDIBits(
            dc,
            (r.right - width) / 2,
            (canvas_h - height) / 2,
            width,
            height,
            0,
            0,
            image.width() as i32,
            image.height() as i32,
            Some(image.bgra.as_ptr() as *const _),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        SetStretchBltMode(dc, STRETCH_BLT_MODE(old));
    }
}
pub(super) fn preview(image: &Image, scale: f32) -> Result<Image> {
    unsafe {
        let w = (700.0 * scale) as i32;
        let h = (500.0 * scale) as i32;
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
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) => b,
            Err(e) => {
                let _ = DeleteDC(dc);
                return Err(e);
            }
        };
        let old = SelectObject(dc, bitmap.into());
        let font = super::font(scale);
        let r = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let pin = Pin::new(image.width(), image.height(), (w, h - footer(scale)));
        draw(dc, r, image, &pin, scale);
        for (id, r) in ACTIONS.into_iter().zip(buttons(r, scale)) {
            button(dc, r, id, &pin, font, id == COPY, false, 0.0);
        }
        let _ = GdiFlush();
        let mut pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        for p in pixels.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(dc);
        Ok(Image::new(kadr_doc::Rect::new(0, 0, w, h), pixels))
    }
}
