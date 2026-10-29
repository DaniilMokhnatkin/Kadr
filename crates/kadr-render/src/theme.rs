//! Токены дизайна. Направление — editorial / Vercel (awesome-claude-design): чистый грейскейл,
//! рамки в 1 px, радиусы ≤ 8, без теней и градиентов. Цвет на экране только у самого скрина —
//! это данные (taste-skill: «colour goes on data, not chrome»), хром его не повторяет и с ним не спорит.
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
    /// Основной текст и цифры: 16:1 на `surface`.
    pub text: D2D1_COLOR_F,
    /// Подписи и единицы: 7.3:1 на `surface`.
    pub text_muted: D2D1_COLOR_F,
    /// Рамка выделения. Снаружи неё всегда затемнение, поэтому белая линия контрастна
    /// на любом скрине (от 3.6:1 на белом до 21:1 на чёрном).
    pub frame: D2D1_COLOR_F,
    /// Тёмная волосяная линия снаружи рамки: отделяет её от светлого содержимого.
    pub frame_outline: D2D1_COLOR_F,
}

impl Theme {
    pub fn current() -> Self {
        if high_contrast_on() { Self::high_contrast() } else { Self::editorial() }
    }

    fn editorial() -> Self {
        Self {
            scrim: rgb(0x000000, 0.5),
            surface: rgb(0x111111, 1.0),
            border: rgb(0x2e2e2e, 1.0),
            text: rgb(0xededed, 1.0),
            text_muted: rgb(0xa1a1a1, 1.0),
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
            text,
            text_muted: text,
            frame: sys(COLOR_HIGHLIGHT),
            frame_outline: window,
        }
    }
}

/// Размеры в DIP: при отрисовке умножаются на масштаб монитора. Сетка 4 px, радиусы ≤ 8.
pub mod dip {
    /// Роль focus ring: 2–4 px (ui-ux-pro-max). 1.5 DIP = 2 px при 100–150 %, 3 px при 200 %.
    pub const FRAME: f32 = 1.5;
    pub const FRAME_OUTLINE: f32 = 1.0;
    /// Сторона маркера на рамке выделения.
    pub const HANDLE: f32 = 6.0;
    /// Насколько далеко от границы выделения курсор ещё «берёт» сторону или угол.
    pub const GRAB: f32 = 8.0;
    pub const BORDER: f32 = 1.0;
    pub const RADIUS_SM: f32 = 4.0;
    pub const RADIUS_MD: f32 = 8.0;
    pub const SPACE_1: f32 = 4.0;
    pub const SPACE_2: f32 = 8.0;
    pub const SPACE_3: f32 = 12.0;
    pub const SPACE_4: f32 = 16.0;
    /// Подписи и цифры. Не мельче 12: текст меньше 12 px ui-ux-pro-max считает антипаттерном.
    pub const TEXT: f32 = 12.0;
    pub const TITLE: f32 = 13.0;
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
