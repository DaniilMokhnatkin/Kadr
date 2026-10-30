//! Kadr's own clipboard history, behind Win+V. Every change of the clipboard is read at once on the
//! UI thread (only the bytes are copied out), and a worker turns it into an entry: pictures become
//! PNG with a small preview, everything is sealed to disk by `kadr_store::clips`, so pinned entries
//! and the latest ones are there again after a restart. What a password manager marks as «keep out
//! of history» is never read.
use kadr_doc::{Image, Rect, pixels};
use kadr_store::clips::{Clips, Data};
use kadr_win::{log_info, log_warn};
use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{PCWSTR, w};

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
/// Longer text is cut to this many characters; nobody pastes a book from history.
const MAX_TEXT: usize = 1 << 20;
/// Pictures larger than this many pixels are not kept (a whole 8K screen still fits).
const MAX_PIXELS: usize = 40_000_000;
/// The preview in the list.
const THUMB: (u32, u32) = (360, 220);

/// What was on the clipboard, as copied out of it.
pub enum Raw {
    Text(String),
    Png(Vec<u8>),
    Dib(Vec<u8>),
    Image(Arc<Image>),
    Files(Vec<String>),
}

/// An entry as the window shows it.
#[derive(Clone)]
pub struct Entry {
    pub id: u64,
    pub created_ms: u64,
    pub pinned: bool,
    pub kind: Kind,
}
#[derive(Clone)]
pub enum Kind {
    Text(String),
    /// The preview and the picture's own size.
    Image(Option<Arc<Image>>, u32, u32),
    Files(Vec<String>),
}

enum Job {
    Add(Raw),
    Pin(u64, bool),
    Remove(u64),
    Touch(u64),
    Clear,
    Max(usize),
}

pub struct History {
    store: Arc<Mutex<Option<Clips<crate::recent_worker::Dpapi>>>>,
    thumbs: Arc<Mutex<HashMap<u64, Arc<Image>>>>,
    tx: Sender<Job>,
}

impl History {
    /// Opens the history on a worker; `message` is posted to `host` whenever the list changes.
    pub fn start(host: HWND, message: u32, max: usize) -> Self {
        let store = Arc::new(Mutex::new(None));
        let thumbs = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = channel::<Job>();
        let (s, t, host) = (store.clone(), thumbs.clone(), host.0 as isize);
        std::thread::spawn(move || {
            let changed = || unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), message, WPARAM(0), LPARAM(0));
            };
            let Some(root) =
                std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("Kadr").join("clips"))
            else {
                return;
            };
            match Clips::open(root, crate::recent_worker::Dpapi, max) {
                Ok(clips) => {
                    let previews: Vec<(u64, Vec<u8>, u32, u32)> = clips
                        .items()
                        .iter()
                        .filter_map(|c| match &c.data {
                            Data::Image { width, height, png } => Some((c.id, png.clone(), *width, *height)),
                            _ => None,
                        })
                        .collect();
                    log_info!("история буфера: {} записей", clips.items().len());
                    *s.lock().unwrap() = Some(clips);
                    changed();
                    for (id, png, w, h) in previews {
                        if let Some(thumb) = preview(&png, w, h) {
                            t.lock().unwrap().insert(id, thumb);
                        }
                    }
                    changed();
                }
                Err(e) => log_warn!("история буфера не открылась: {e}"),
            }
            for job in rx {
                let now = crate::recent_worker::now_ms();
                // Converting a picture happens before the lock: the window can read meanwhile.
                let result = match job {
                    Job::Add(raw) => match into_data(raw) {
                        Some((data, thumb)) => {
                            let added = s.lock().unwrap().as_mut().map(|c| c.add(data, now));
                            match (added, thumb) {
                                (Some(Ok(id)), Some(thumb)) => {
                                    t.lock().unwrap().insert(id, thumb);
                                    Ok(())
                                }
                                (Some(r), _) => r.map(|_| ()),
                                (None, _) => Ok(()),
                            }
                        }
                        None => continue,
                    },
                    Job::Pin(id, on) => with(&s, |c| c.set_pinned(id, on)),
                    Job::Remove(id) => with(&s, |c| c.remove(id)),
                    Job::Touch(id) => with(&s, |c| c.touch(id, now)),
                    Job::Clear => with(&s, |c| c.clear()),
                    Job::Max(n) => with(&s, |c| c.set_max(n)),
                };
                if let Err(e) = result {
                    log_warn!("история буфера: {e}");
                }
                // Previews of entries that are gone go too.
                if let Some(clips) = s.lock().unwrap().as_ref() {
                    t.lock().unwrap().retain(|id, _| clips.get(*id).is_some());
                }
                changed();
            }
        });
        Self { store, thumbs, tx }
    }

    pub fn add(&self, raw: Raw) {
        let _ = self.tx.send(Job::Add(raw));
    }
    pub fn pin(&self, id: u64, on: bool) {
        let _ = self.tx.send(Job::Pin(id, on));
    }
    pub fn remove(&self, id: u64) {
        let _ = self.tx.send(Job::Remove(id));
    }
    pub fn touch(&self, id: u64) {
        let _ = self.tx.send(Job::Touch(id));
    }
    pub fn clear(&self) {
        let _ = self.tx.send(Job::Clear);
    }
    pub fn set_max(&self, max: usize) {
        let _ = self.tx.send(Job::Max(max));
    }

    /// The list for the window, newest first, pinned ones included where they fall.
    pub fn entries(&self) -> Vec<Entry> {
        let store = self.store.lock().unwrap();
        let thumbs = self.thumbs.lock().unwrap();
        let Some(clips) = store.as_ref() else { return Vec::new() };
        clips
            .items()
            .iter()
            .map(|c| Entry {
                id: c.id,
                created_ms: c.created_ms,
                pinned: c.pinned,
                kind: match &c.data {
                    Data::Text(t) => Kind::Text(t.clone()),
                    Data::Image { width, height, .. } => Kind::Image(thumbs.get(&c.id).cloned(), *width, *height),
                    Data::Files(f) => Kind::Files(f.clone()),
                },
            })
            .collect()
    }

    /// What to put back on the clipboard.
    pub fn data(&self, id: u64) -> Option<Data> {
        self.store.lock().unwrap().as_ref()?.get(id).map(|c| c.data.clone())
    }
}

fn with(
    store: &Mutex<Option<Clips<crate::recent_worker::Dpapi>>>,
    f: impl FnOnce(&mut Clips<crate::recent_worker::Dpapi>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    store.lock().unwrap().as_mut().map_or(Ok(()), f)
}

/// The entry and, for a picture, its preview.
fn into_data(raw: Raw) -> Option<(Data, Option<Arc<Image>>)> {
    let image = match raw {
        Raw::Text(t) => {
            let t: String = t.chars().take(MAX_TEXT).collect();
            return (!t.trim().is_empty()).then_some((Data::Text(t), None));
        }
        Raw::Files(f) => return (!f.is_empty()).then_some((Data::Files(f), None)),
        Raw::Png(png) => {
            // Kept as it came; only read to learn its size and draw the preview.
            let size = png_size(&png)?;
            let thumb = preview(&png, size.0, size.1);
            return Some((Data::Image { width: size.0, height: size.1, png }, thumb));
        }
        Raw::Dib(dib) => Arc::new(from_dib(&dib)?),
        Raw::Image(image) => image,
    };
    let (w, h) = (image.width(), image.height());
    let png = pixels::encode_png(&image).ok()?;
    let thumb = Arc::new(pixels::thumbnail(std::slice::from_ref(&image), image.rect, THUMB.0, THUMB.1));
    Some((Data::Image { width: w, height: h, png }, Some(thumb)))
}

fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    // IHDR right after the signature: width and height, big-endian.
    let w = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    (png.starts_with(b"\x89PNG") && w > 0 && h > 0 && (w as usize) * (h as usize) <= MAX_PIXELS).then_some((w, h))
}

fn preview(png: &[u8], w: u32, h: u32) -> Option<Arc<Image>> {
    let image = Arc::new(pixels::decode_png(png, Rect::new(0, 0, w as i32, h as i32)).ok()?);
    Some(Arc::new(pixels::thumbnail(std::slice::from_ref(&image), image.rect, THUMB.0, THUMB.1)))
}

/// A device-independent bitmap (what most programs put on the clipboard) as BGRA, top to bottom.
fn from_dib(dib: &[u8]) -> Option<Image> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?));
    let header = u32_at(0)? as usize;
    let width = u32_at(4)? as i32;
    let height = u32_at(8)? as i32;
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    let colors = u32_at(32)? as usize;
    if width <= 0 || height == 0 || !matches!(bits, 24 | 32) || !matches!(compression, 0 | 3) {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if w * h > MAX_PIXELS {
        return None;
    }
    // Colour masks follow a plain 40-byte header with BI_BITFIELDS.
    let masks = if compression == 3 && header == 40 { 12 } else { 0 };
    let start = header + masks + colors * 4;
    let stride = (w * bits as usize / 8 + 3) & !3;
    let data = dib.get(start..start + stride * h)?;
    let mut bgra = vec![0u8; w * h * 4];
    let mut transparent = true;
    for y in 0..h {
        let row = if height > 0 { h - 1 - y } else { y };
        let src = &data[row * stride..];
        for x in 0..w {
            let o = (y * w + x) * 4;
            if bits == 32 {
                bgra[o..o + 4].copy_from_slice(&src[x * 4..x * 4 + 4]);
                transparent &= src[x * 4 + 3] == 0;
            } else {
                bgra[o..o + 3].copy_from_slice(&src[x * 3..x * 3 + 3]);
                bgra[o + 3] = 255;
            }
        }
    }
    // Most programs leave alpha at zero in a 32-bit DIB: that means opaque, not invisible.
    if bits == 32 && transparent {
        bgra.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    Some(Image::new(Rect::new(0, 0, w as i32, h as i32), bgra))
}

/// Reads what was just copied. `None` when there is nothing to keep or it asks not to be kept.
pub fn read(owner: HWND) -> Option<Raw> {
    let format = |name: PCWSTR| unsafe { RegisterClipboardFormatW(name) };
    // Opening can fail while the program that copied still holds the clipboard: try a moment.
    let mut opened = false;
    for _ in 0..4 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    if !opened {
        return None;
    }
    let result = (|| unsafe {
        // Password managers and the like mark what must not end up in a history.
        let private = [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("Clipboard Viewer Ignore")]
            .into_iter()
            .any(|n| IsClipboardFormatAvailable(format(n)).is_ok());
        if private {
            return None;
        }
        let allow = format(w!("CanIncludeInClipboardHistory"));
        if IsClipboardFormatAvailable(allow).is_ok()
            && let Some(bytes) = bytes(allow, 16)
            && bytes.get(..4).is_some_and(|b| b == [0, 0, 0, 0])
        {
            return None;
        }
        if IsClipboardFormatAvailable(CF_HDROP).is_ok()
            && let Some(files) = bytes(CF_HDROP, 1 << 20).and_then(|b| drop_files(&b))
        {
            return Some(Raw::Files(files));
        }
        let png = crate::clip_history::png_format();
        if IsClipboardFormatAvailable(png).is_ok()
            && let Some(b) = bytes(png, 256 << 20)
        {
            return Some(Raw::Png(b));
        }
        if IsClipboardFormatAvailable(CF_DIB).is_ok()
            && let Some(b) = bytes(CF_DIB, 256 << 20)
        {
            return Some(Raw::Dib(b));
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok()
            && let Some(b) = bytes(CF_UNICODETEXT, MAX_TEXT * 2 + 2)
        {
            let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
            return Some(Raw::Text(String::from_utf16_lossy(&units[..end])));
        }
        None
    })();
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

fn png_format() -> u32 {
    kadr_export::clipboard::png_format()
}

/// Up to `limit` bytes of a clipboard format; the clipboard is open.
unsafe fn bytes(format: u32, limit: usize) -> Option<Vec<u8>> {
    unsafe {
        let handle = HGLOBAL(GetClipboardData(format).ok()?.0);
        let size = GlobalSize(handle);
        if size == 0 || size > limit {
            return None;
        }
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let out = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(handle);
        Some(out)
    }
}

/// DROPFILES: an offset to the names, then wide (or ANSI) names, each ended by a zero.
fn drop_files(b: &[u8]) -> Option<Vec<String>> {
    let offset = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
    let wide = u32::from_le_bytes(b.get(16..20)?.try_into().ok()?) != 0;
    let list = b.get(offset..)?;
    let names: Vec<String> = if wide {
        let units: Vec<u16> = list.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        units.split(|u| *u == 0).take_while(|n| !n.is_empty()).map(String::from_utf16_lossy).collect()
    } else {
        list.split(|c| *c == 0).take_while(|n| !n.is_empty()).map(|n| String::from_utf8_lossy(n).into_owned()).collect()
    };
    (!names.is_empty()).then_some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bottom_up_dib_reads_top_to_bottom_and_zero_alpha_means_opaque() {
        // 2×2, 32 bits, bottom row first: blue, green / red, white.
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&2i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib.extend([255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 255, 255, 255, 0]);
        let image = from_dib(&dib).unwrap();
        assert_eq!((image.width(), image.height()), (2, 2));
        assert_eq!(&image.bgra[..8], [0, 0, 255, 255, 255, 255, 255, 255], "top row: red, white");
        assert_eq!(&image.bgra[8..], [255, 0, 0, 255, 0, 255, 0, 255], "bottom row: blue, green");
    }

    #[test]
    fn a_24_bit_dib_with_padded_rows() {
        // 1×2, 24 bits: each row is padded to four bytes.
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&(-2i32).to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        let image = from_dib(&dib).unwrap();
        assert_eq!(image.bgra, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn explorer_file_lists_and_png_sizes() {
        let mut b = 20u32.to_le_bytes().to_vec();
        b.extend([0; 12]);
        b.extend(1u32.to_le_bytes());
        b.extend(
            r"C:\a.txt"
                .encode_utf16()
                .chain([0])
                .chain(r"D:\Фото\b.png".encode_utf16())
                .chain([0, 0])
                .flat_map(u16::to_le_bytes),
        );
        assert_eq!(drop_files(&b).unwrap(), [r"C:\a.txt", r"D:\Фото\b.png"]);
        let png = pixels::encode_png(&Image::new(Rect::new(0, 0, 3, 2), vec![9; 24])).unwrap();
        assert_eq!(png_size(&png), Some((3, 2)));
        let (data, thumb) = into_data(Raw::Png(png.clone())).unwrap();
        assert_eq!(data, Data::Image { width: 3, height: 2, png });
        assert_eq!(thumb.map(|t| (t.width(), t.height())), Some((3, 2)));
        assert!(into_data(Raw::Text("   \n".into())).is_none());
    }
}
