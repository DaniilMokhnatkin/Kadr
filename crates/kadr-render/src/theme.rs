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

static MARCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_marching_ants(enabled: bool) {
    MARCHING.store(enabled, std::sync::atomic::Ordering::Relaxed);
}
pub fn marching_ants() -> bool {
    MARCHING.load(std::sync::atomic::Ordering::Relaxed)
}

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
    /// Tint over the blurred screenshot behind the toolbar. Alpha 1 turns the material opaque.
    pub glass: D2D1_COLOR_F,
    /// Edge of floating surfaces; a darker line than `border`, but translucent so it reads on any backdrop.
    pub hairline: D2D1_COLOR_F,
    /// Light rim just inside the edge.
    pub sheen: D2D1_COLOR_F,
    /// Shadow ink; alpha 0 disables shadows (high contrast).
    pub shadow: D2D1_COLOR_F,
    /// Wash over the window material (acrylic/Mica) so text keeps its contrast.
    pub wash: D2D1_COLOR_F,
    /// Grouped list card in Settings.
    pub group: D2D1_COLOR_F,
    /// Neutral translucent fill for secondary buttons and chips, and its hover.
    pub tint: D2D1_COLOR_F,
    pub tint_strong: D2D1_COLOR_F,
    /// Switch track when off.
    pub track: D2D1_COLOR_F,
    /// Placeholder behind thumbnails.
    pub tile: D2D1_COLOR_F,
    /// Dark appearance: windows ask DWM for the dark material too.
    pub dark: bool,
}

impl Theme {
    pub fn current() -> Self {
        if high_contrast_on() {
            Self::high_contrast()
        } else if apps_use_dark() {
            Self::porcelain_dark()
        } else {
            Self::porcelain()
        }
    }

    /// The same roles at night: graphite surfaces, white ink, the accent unchanged.
    fn porcelain_dark() -> Self {
        Self {
            scrim: rgb(0x000000, 0.48),
            surface: rgb(0x1c1c1e, 1.0),
            border: rgb(0x3a3a3c, 1.0),
            control: rgb(0x28282b, 1.0),
            selected: rgb(0x0b3a6e, 1.0),
            on_selected: rgb(0x66b0ff, 1.0),
            accent: rgb(0x0071e3, 1.0),
            on_accent: rgb(0xffffff, 1.0),
            disabled: rgb(0x636366, 1.0),
            text: rgb(0xf5f5f7, 1.0),
            text_muted: rgb(0xaeaeb2, 1.0),
            frame: rgb(0xffffff, 1.0),
            frame_outline: rgb(0x000000, 0.6),
            glass: rgb(0x1c1c1e, 0.80),
            hairline: rgb(0xffffff, 0.12),
            sheen: rgb(0xffffff, 0.07),
            shadow: rgb(0x000000, 1.0),
            wash: rgb(0x1c1c1e, 0.42),
            group: rgb(0xffffff, 0.06),
            tint: rgb(0xffffff, 0.08),
            tint_strong: rgb(0xffffff, 0.14),
            track: rgb(0x787880, 0.40),
            tile: rgb(0x2c2c2e, 1.0),
            dark: true,
        }
    }

    fn porcelain() -> Self {
        Self {
            scrim: rgb(0x000000, 0.42),
            surface: rgb(0xfbfbfd, 1.0),
            border: rgb(0xd6d6dc, 1.0),
            // Pills sit on glass: nearly opaque so contrast does not depend on the screenshot.
            control: rgb(0xececf0, 1.0),
            selected: rgb(0xdbe7fb, 1.0),
            on_selected: rgb(0x005fcc, 1.0),
            accent: rgb(0x0071e3, 1.0),
            on_accent: rgb(0xffffff, 1.0),
            disabled: rgb(0x929298, 1.0),
            text: rgb(0x1d1d1f, 1.0),
            // Darker than before: on glass over a black screen the worst backdrop is ~#d7d7d9.
            text_muted: rgb(0x56565c, 1.0),
            frame: rgb(0xffffff, 1.0),
            frame_outline: rgb(0x000000, 0.6),
            glass: rgb(0xf7f7f9, 0.84),
            hairline: rgb(0x000000, 0.13),
            sheen: rgb(0xffffff, 0.55),
            shadow: rgb(0x000000, 1.0),
            wash: rgb(0xffffff, 0.42),
            group: rgb(0xffffff, 0.78),
            tint: rgb(0x000000, 0.06),
            tint_strong: rgb(0x000000, 0.11),
            track: rgb(0x787880, 0.32),
            tile: rgb(0xe9e9ee, 1.0),
            dark: false,
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
            glass: window,
            hairline: text,
            sheen: rgb(0x000000, 0.0),
            shadow: rgb(0x000000, 0.0),
            wash: window,
            group: window,
            tint: fade_of(text, 0.12),
            tint_strong: fade_of(text, 0.22),
            track: fade_of(text, 0.4),
            tile: window,
            dark: false,
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

fn fade_of(mut c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    c.a = a;
    c
}

/// The appearance chosen in Settings: 0 follows Windows, 1 light, 2 dark.
static CHOSEN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `None` follows Windows; `Some(true)` is always dark, `Some(false)` always light.
pub fn choose(dark: Option<bool>) {
    let v = match dark {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    CHOSEN.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Windows «app mode: dark» (Personalize\AppsUseLightTheme = 0), unless Settings say otherwise.
fn apps_use_dark() -> bool {
    // Design snapshots render both appearances without touching the Windows setting.
    match std::env::var("KADR_APPEARANCE").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    match CHOSEN.load(std::sync::atomic::Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut value = 1u32;
    let mut size = 4u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    ok.is_ok() && value == 0
}

fn high_contrast_on() -> bool {
    let mut hc = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
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
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn porcelain_text_and_selected_and_primary_controls_keep_their_contrast() {
        for t in [Theme::porcelain(), Theme::porcelain_dark()] {
            for (ink, paper) in [
                (t.text, t.surface),
                (t.text_muted, t.surface),
                (t.text, t.control),
                (t.text_muted, t.control),
                (t.on_accent, t.accent),
                (t.on_selected, t.selected),
            ] {
                assert!(contrast(ink, paper) >= 4.5, "dark {}: {} on {:?}", t.dark, contrast(ink, paper), paper);
            }
        }
        let hc = Theme::high_contrast();
        assert_eq!(hc.on_selected, sys(windows::Win32::Graphics::Gdi::COLOR_HIGHLIGHTTEXT));
    }
}
