//! Kadr / Porcelain. Apple reference: quiet light surfaces, optical spacing, one action tint.
//! Opaque materials keep contrast independent of the screenshot and preserve the input budget.
//! Контраст проверен по правилам ui-ux-pro-max (текст ≥ 4.5:1, не-текстовые элементы ≥ 3:1).
//! В коде отрисовки — только роли отсюда, никаких «сырых» цветов.

use std::ffi::c_void;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Gdi::{COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

#[derive(Clone, Copy)]
pub struct Theme {
    /// Затемнение замороженного экрана вне выделения.
    pub scrim: D2D1_COLOR_F,
    /// Непрозрачные панели (HUD, подпись размера): контраст текста не зависит от скрина под ними.
    pub surface: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    pub control: D2D1_COLOR_F,
    pub selected: D2D1_COLOR_F,
    pub on_selected: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    pub on_accent: D2D1_COLOR_F,
    pub disabled: D2D1_COLOR_F,
    /// Основной текст и цифры.
    pub text: D2D1_COLOR_F,
    /// Вторичный текст с контрастом не менее 4.5:1 на surface и control.
    pub text_muted: D2D1_COLOR_F,
    /// Рамка выделения. Снаружи неё всегда затемнение, поэтому белая линия контрастна
    /// на любом скрине (от 3:1 на белом до 21:1 на чёрном).
    pub frame: D2D1_COLOR_F,
    /// Тёмная волосяная линия снаружи рамки: отделяет её от светлого содержимого.
    pub frame_outline: D2D1_COLOR_F,
}

impl Theme {
    pub fn current() -> Self {
        if high_contrast_on() {
            Self::high_contrast()
        } else {
            Self::porcelain()
        }
    }

    fn porcelain() -> Self {
        Self {
            scrim: rgb(0x000000, 0.42),
            surface: rgb(0xfafafc, 1.0),
            border: rgb(0xd6d6dc, 1.0),
            control: rgb(0xeeeef2, 1.0),
            selected: rgb(0xe0edff, 1.0),
            on_selected: rgb(0x005fcc, 1.0),
            accent: rgb(0x005fcc, 1.0),
            on_accent: rgb(0xffffff, 1.0),
            disabled: rgb(0x929298, 1.0),
            text: rgb(0x1d1d1f, 1.0),
            text_muted: rgb(0x636369, 1.0),
            frame: rgb(0xffffff, 1.0),
            frame_outline: rgb(0x000000, 0.6),
        }
    }

    /// Режим высокой контрастности Windows: только системные цвета.
    fn high_contrast() -> Self {
        let window = sys(COLOR_WINDOW);
        let text = sys(COLOR_WINDOWTEXT);
        Self {
            scrim: rgb(0x000000, 0.5),
            surface: window,
            border: text,
            control: window,
            selected: sys(COLOR_HIGHLIGHT),
            on_selected: sys(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHTTEXT),
            accent: sys(COLOR_HIGHLIGHT),
            on_accent: sys(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHTTEXT),
            disabled: text,
            text,
            text_muted: text,
            frame: sys(COLOR_HIGHLIGHT),
            frame_outline: window,
        }
    }
}

/// Размеры в DIP: при отрисовке умножаются на масштаб монитора. Сетка 4/8 px.
pub mod dip {
    /// Роль focus ring: 2–4 px (ui-ux-pro-max). 1.5 DIP = 2 px при 100–150 %, 3 px при 200 %.
    pub const FRAME: f32 = 1.5;
    pub const FRAME_OUTLINE: f32 = 1.0;
    /// Сторона маркера на рамке выделения.
    pub const HANDLE: f32 = 6.0;
    /// Насколько далеко от границы выделения курсор ещё «берёт» сторону или угол.
    pub const GRAB: f32 = 8.0;
    pub const BORDER: f32 = 1.0;
    pub const RADIUS_SM: f32 = 8.0;
    pub const RADIUS_MD: f32 = 14.0;
    pub const SPACE_1: f32 = 4.0;
    pub const SPACE_2: f32 = 8.0;
    pub const SPACE_3: f32 = 12.0;
    pub const SPACE_4: f32 = 16.0;
    /// Подписи и цифры. Не мельче 12: текст меньше 12 px ui-ux-pro-max считает антипаттерном.
    pub const TEXT: f32 = 13.0;
    pub const TITLE: f32 = 14.0;
    /// Главная цифра панели — ответ на её вопрос, вдвое крупнее остальных.
    pub const LEAD: f32 = 24.0;
}

/// Одно семейство — системный гротеск Windows 11. Цифры в нём табличные (OpenType `tnum`),
/// поэтому стоят разряд под разрядом и не прыгают при обновлении.
pub const UI_FONTS: &[&str] = &["Segoe UI Variable Text", "Segoe UI"];

const fn rgb(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}

fn sys(index: SYS_COLOR_INDEX) -> D2D1_COLOR_F {
    let c = unsafe { GetSysColor(index) }; // 0x00BBGGRR
    D2D1_COLOR_F {
        r: (c & 0xff) as f32 / 255.0,
        g: ((c >> 8) & 0xff) as f32 / 255.0,
        b: ((c >> 16) & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

fn high_contrast_on() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some(&mut hc as *mut HIGHCONTRASTW as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    ok && hc.dwFlags.contains(HCF_HIGHCONTRASTON)
}

/// Windows "Animation effects" preference. A changed preference snaps to the semantic state.
pub fn motion_enabled() -> bool {
    let mut enabled = windows::core::BOOL(1);
    let _ = unsafe {
        SystemParametersInfoW(
            windows::Win32::UI::WindowsAndMessaging::SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut enabled as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    enabled.as_bool() && !high_contrast_on()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contrast(a: D2D1_COLOR_F, b: D2D1_COLOR_F) -> f32 {
        let luminance = |c: D2D1_COLOR_F| {
            let f = |v: f32| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn porcelain_text_and_selected_and_primary_controls_keep_their_contrast() {
        let t = Theme::porcelain();
        for (ink, paper) in [
            (t.text, t.surface),
            (t.text_muted, t.surface),
            (t.text, t.control),
            (t.text_muted, t.control),
            (t.on_accent, t.accent),
            (t.on_selected, t.selected),
        ] {
            assert!(contrast(ink, paper) >= 4.5);
        }
        let hc = Theme::high_contrast();
        assert_eq!(hc.on_selected, sys(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHTTEXT));
    }
}
