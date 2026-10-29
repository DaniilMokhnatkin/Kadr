//! Общий знак приложения и монохромные варианты трея. HICON живут дольше всех окон App.
use std::ffi::c_void;
use windows::Win32::Foundation::{ERROR_SUCCESS, HINSTANCE};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColor};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

struct Icon(HICON);
impl Icon {
    fn load(instance: HINSTANCE, id: u16, size: i32) -> Result<Self> {
        unsafe {
            LoadImageW(
                Some(instance),
                PCWSTR(id as usize as *const u16),
                IMAGE_ICON,
                size,
                size,
                IMAGE_FLAGS::default(),
            )
            .map(|h| Self(HICON(h.0)))
        }
    }
}
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}
pub struct Icons {
    app: Icon,
    small: Icon,
    dark: Icon,
    light: Icon,
}
impl Icons {
    pub fn load(instance: HINSTANCE) -> Result<Self> {
        let small = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let large = unsafe { GetSystemMetrics(SM_CXICON) };
        Ok(Self {
            app: Icon::load(instance, 1, large)?,
            small: Icon::load(instance, 1, small)?,
            dark: Icon::load(instance, 101, small)?,
            light: Icon::load(instance, 102, small)?,
        })
    }
    pub fn app(&self) -> HICON {
        self.app.0
    }
    pub fn small(&self) -> HICON {
        self.small.0
    }
    pub fn tray(&self) -> HICON {
        if light_taskbar() { self.light.0 } else { self.dark.0 }
    }
}
fn light_taskbar() -> bool {
    let mut contrast = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    if unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some(&mut contrast as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
        && contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
    {
        let bg = unsafe { GetSysColor(COLOR_WINDOW) };
        return ((bg & 255) * 299 + ((bg >> 8) & 255) * 587 + ((bg >> 16) & 255) * 114) > 128000;
    }
    let mut value = 0u32;
    let mut size = 4u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    result == ERROR_SUCCESS && value != 0
}
