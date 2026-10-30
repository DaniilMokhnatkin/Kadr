//! Сохранение в файл: папка по умолчанию, имя по времени, системный диалог «Сохранить как».

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IBindCtx};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOLDERID_Pictures, FOLDERID_Videos, FileSaveDialog, IFileSaveDialog, IShellItem, KF_FLAG_DEFAULT,
    SHCreateItemFromParsingName, SHGetKnownFolderPath, SIGDN_FILESYSPATH,
};
use windows::core::{IUnknown, PCWSTR, PWSTR, Result, w};

use kadr_doc::Image;
use kadr_win::wide;

/// «Изображения\Screenshots» — туда же складывает снимки сама Windows.
pub fn default_folder() -> PathBuf {
    let pictures = unsafe { SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) }
        .ok()
        .map(|p| PathBuf::from(take_string(p)))
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Pictures"));
    pictures.join("Screenshots")
}

/// «Видео\Kadr»: screen recordings.
pub fn video_folder() -> PathBuf {
    let videos = unsafe { SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None) }
        .ok()
        .map(|p| PathBuf::from(take_string(p)))
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos"));
    videos.join("Kadr")
}

/// `Screenshot_2026-10-29_18-45-21` по местному времени.
pub fn timestamp_name() -> String {
    let t = unsafe { GetLocalTime() };
    format!("Screenshot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// `<stem>.png`, а если такой файл уже есть — `<stem>_2.png`, `<stem>_3.png`…
pub fn unique_path(folder: &Path, stem: &str) -> PathBuf {
    let mut path = folder.join(format!("{stem}.png"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("{stem}_{n}.png"));
        n += 1;
    }
    path
}

/// Пишет PNG через временный файл и переименование: полупустого снимка на диске не бывает.
pub fn write_png(path: &Path, image: &Image) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = crate::encode_png(image).map_err(std::io::Error::other)?;
    let partial = path.with_extension("png.part");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)
}

/// Системный диалог «Сохранить как». `Ok(None)` — пользователь передумал.
pub fn ask_path(folder: &Path, stem: &str) -> Result<Option<PathBuf>> {
    unsafe {
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None::<&IUnknown>, CLSCTX_INPROC_SERVER)?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: w!("PNG"), pszSpec: w!("*.png") }])?;
        dialog.SetDefaultExtension(w!("png"))?;
        let name = wide(stem);
        dialog.SetFileName(PCWSTR(name.as_ptr()))?;
        let _ = std::fs::create_dir_all(folder);
        let folder = wide(&folder.to_string_lossy());
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(folder.as_ptr()), None::<&IBindCtx>) {
            let _ = dialog.SetFolder(&item);
        }
        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            Err(e) => return Err(e),
        }
        let path = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        Ok(Some(PathBuf::from(take_string(path))))
    }
}

/// Строка, выделенная оболочкой через CoTaskMemAlloc: копируем и освобождаем.
fn take_string(p: PWSTR) -> String {
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_skips_existing_files() {
        let dir = std::env::temp_dir().join(format!("kadr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "shot"), dir.join("shot.png"));
        std::fs::write(dir.join("shot.png"), b"x").unwrap();
        std::fs::write(dir.join("shot_2.png"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "shot"), dir.join("shot_3.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn timestamp_name_shape() {
        let name = timestamp_name();
        assert!(name.starts_with("Screenshot_20") && name.len() == "Screenshot_2026-10-29_18-45-21".len());
    }
}
