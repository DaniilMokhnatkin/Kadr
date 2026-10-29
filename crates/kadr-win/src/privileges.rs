//! Уровень целостности, uiAccess и процесс на переднем плане — данные для эксперимента M0
//! с элевированными окнами (диспетчер задач).

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_INFORMATION_CLASS,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel, TokenUIAccess,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{PWSTR, w};

pub struct ProcessInfo {
    pub integrity: &'static str,
    pub ui_access: bool,
    pub elevated: bool,
}

pub fn current() -> ProcessInfo {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return ProcessInfo { integrity: "?", ui_access: false, elevated: false };
        }
        let info = ProcessInfo {
            integrity: integrity_level(token),
            ui_access: query_u32(token, TokenUIAccess).unwrap_or(0) != 0,
            elevated: query_u32(token, TokenElevation).unwrap_or(0) != 0,
        };
        let _ = CloseHandle(token);
        info
    }
}

unsafe fn query_u32(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Option<u32> {
    let mut value = 0u32;
    let mut len = 0u32;
    let ptr = &mut value as *mut u32 as *mut c_void;
    unsafe { GetTokenInformation(token, class, Some(ptr), 4, &mut len) }.ok()?;
    Some(value)
}

unsafe fn integrity_level(token: HANDLE) -> &'static str {
    // u64 — ради выравнивания TOKEN_MANDATORY_LABEL внутри буфера.
    let mut buf = [0u64; 16];
    let mut len = 0u32;
    let ptr = buf.as_mut_ptr() as *mut c_void;
    if unsafe { GetTokenInformation(token, TokenIntegrityLevel, Some(ptr), 128, &mut len) }.is_err() {
        return "?";
    }
    let rid = unsafe {
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        *GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1)
    };
    match rid {
        r if r >= 0x4000 => "System",
        r if r >= 0x3000 => "High",
        r if r >= 0x2000 => "Medium",
        r if r >= 0x1000 => "Low",
        _ => "Untrusted",
    }
}

/// Имя exe окна на переднем плане. Для элевированных процессов тоже работает:
/// PROCESS_QUERY_LIMITED_INFORMATION не упирается в уровень целостности.
pub fn foreground_exe() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return "—".into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return format!("pid {pid}");
        };
        let mut buf = [0u16; 260];
        let mut size = buf.len() as u32;
        let name = match QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size) {
            Ok(()) => {
                let full = String::from_utf16_lossy(&buf[..size as usize]);
                full.rsplit('\\').next().unwrap_or_default().to_string()
            }
            Err(_) => format!("pid {pid}"),
        };
        let _ = CloseHandle(process);
        name
    }
}

/// Забирают ли «Ножницы» клавишу PrtSc. `None` — значение не задано (в Win11 по умолчанию включено).
pub fn snipping_owns_prtsc() -> Option<bool> {
    let mut value = 0u32;
    let mut size = 4u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Keyboard"),
            w!("PrintScreenKeyForSnippingEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    (err == ERROR_SUCCESS).then_some(value != 0)
}
