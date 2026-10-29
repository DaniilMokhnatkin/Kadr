//! Транзакционное хранение черновиков. Кадры неизменяемы, выделение обновляется отдельно.
use kadr_doc::{Image, Rect, pixels};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_FILE: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_PIXELS: usize = 64 * 1024 * 1024;
pub trait Crypto: Send + 'static {
    fn seal(&self, data: &[u8]) -> io::Result<Vec<u8>>;
    fn open(&self, data: &[u8]) -> io::Result<Vec<u8>>;
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub version: u32,
    pub revision: u64,
    pub selection: Option<[i32; 4]>,
    #[serde(default)]
    pub automatic: bool,
    pub layout: Vec<[i32; 4]>,
    #[serde(default)]
    pub annotations: Vec<kadr_doc::annotation::Annotation>,
}
impl Document {
    pub fn selection_rect(&self) -> Option<Rect> {
        self.selection.map(from_array)
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != 1
            || self.revision == 0
            || self.layout.is_empty()
            || self.layout.len() > 8
            || self.layout.iter().any(|r| !pixels::valid_rect(from_array(*r)))
            || self.selection_rect().is_some_and(|r| !pixels::valid_rect(r))
            || self.annotations.len() > 256
            || self.annotations.iter().any(|s| !s.validate())
            || self.annotations.iter().map(|s| s.points.len()).sum::<usize>() > 65536
        {
            return Err(invalid("неподдерживаемое или повреждённое состояние снимка"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Meta {
    version: u32,
    created_ms: u64,
    open: bool,
}
pub struct Entry {
    pub id: String,
    pub created_ms: u64,
    pub open: bool,
    pub preview: Option<Image>,
}
pub struct Draft {
    pub frames: Vec<Arc<Image>>,
    pub document: Document,
}
pub struct Store<C: Crypto> {
    root: PathBuf,
    crypto: C,
    max: usize,
}
impl<C: Crypto> Store<C> {
    pub fn new(root: PathBuf, crypto: C, max: usize) -> io::Result<Self> {
        if root.exists() && fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(invalid("каталог черновиков не должен быть ссылкой"));
        }
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            crypto,
            max: max.clamp(1, 10),
        })
    }
    fn dir(&self, id: &str) -> io::Result<PathBuf> {
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(invalid("неверное имя черновика"));
        }
        let path = self.root.join(id);
        if path.exists() && fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(invalid("ссылка вместо черновика"));
        }
        Ok(path)
    }
    pub fn create(&self, id: &str, created_ms: u64, frames: &[Arc<Image>], doc: &Document) -> io::Result<()> {
        doc.validate()?;
        let dir = self.dir(id)?;
        if dir.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "черновик уже существует"));
        }
        let pending = self.root.join(format!(".pending-{id}"));
        fs::create_dir(&pending)?;
        let written = (|| -> io::Result<()> {
            let frames = encode_frames(frames)?;
            write_atomic(&pending.join("frames.enc"), &self.crypto.seal(&frames)?)?;
            // Перечитывание кадров не требуется: превью будет записано первым update из рабочего процесса.
            write_atomic(
                &pending.join("state.enc"),
                &self
                    .crypto
                    .seal(toml::to_string(doc).map_err(io::Error::other)?.as_bytes())?,
            )?;
            let meta = Meta {
                version: 1,
                created_ms,
                open: true,
            };
            write_meta(&pending, &meta)?;
            fs::rename(&pending, &dir)
        })();
        if let Err(e) = written {
            // Незавершённая запись не должна копиться на диске; чужие файлы remove_files не тронет.
            let _ = remove_files(&pending);
            return Err(e);
        }
        self.prune()?;
        Ok(())
    }
    pub fn update(&self, id: &str, doc: &Document, open: bool, frames: &[Arc<Image>]) -> io::Result<()> {
        doc.validate()?;
        let dir = self.dir(id)?;
        let mut meta = read_meta(&dir)?;
        if !doc.annotations.is_empty() {
            // Удаляем до state.enc: даже после сбоя карточка не покажет старый исходник.
            match fs::remove_file(dir.join("preview.enc")) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        write_atomic(
            &dir.join("state.enc"),
            &self
                .crypto
                .seal(toml::to_string(doc).map_err(io::Error::other)?.as_bytes())?,
        )?;
        if doc.annotations.is_empty()
            && let Some(bounds) = pixels::bounds(frames)
        {
            let area = doc
                .selection_rect()
                .and_then(|s| s.intersect(&bounds))
                .unwrap_or(bounds);
            let preview = pixels::thumbnail(frames, area, 320, 160);
            self.set_preview(id, &preview)?;
        }
        meta.open = open;
        write_meta(&dir, &meta)
    }
    /// Готовая плоская миниатюра; хранилище не зависит от GPU.
    pub fn set_preview(&self, id: &str, preview: &Image) -> io::Result<()> {
        if preview.width() == 0 || preview.height() == 0 || preview.width() > 320 || preview.height() > 160 {
            return Err(invalid("неверный размер превью"));
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&preview.width().to_le_bytes());
        bytes.extend_from_slice(&preview.height().to_le_bytes());
        bytes.extend_from_slice(&pixels::encode_png(preview).map_err(io::Error::other)?);
        write_atomic(&self.dir(id)?.join("preview.enc"), &self.crypto.seal(&bytes)?)
    }
    pub fn load(&self, id: &str) -> io::Result<Draft> {
        let dir = self.dir(id)?;
        read_meta(&dir)?;
        let document = self.document(&dir)?;
        let frames = decode_frames(&self.crypto.open(&read_limited(&dir.join("frames.enc"), MAX_FILE)?)?)?;
        Ok(Draft { frames, document })
    }
    fn document(&self, dir: &Path) -> io::Result<Document> {
        let bytes = self
            .crypto
            .open(&read_limited(&dir.join("state.enc"), 4 * 1024 * 1024)?)?;
        let doc: Document =
            toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?).map_err(io::Error::other)?;
        doc.validate()?;
        Ok(doc)
    }
    pub fn list(&self) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.root)?.take(1000).flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Ok(dir) = self.dir(&id) else { continue };
            let Ok(meta) = read_meta(&dir) else { continue };
            let preview = (|| -> io::Result<Image> {
                let bytes = self
                    .crypto
                    .open(&read_limited(&dir.join("preview.enc"), 1024 * 1024)?)?;
                if bytes.len() < 8 {
                    return Err(invalid("неполное превью"));
                }
                let w = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                let h = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                if w > 320 || h > 160 {
                    return Err(invalid("слишком большое превью"));
                }
                pixels::decode_png(&bytes[8..], Rect::new(0, 0, w as i32, h as i32))
            })()
            .ok();
            entries.push(Entry {
                id,
                created_ms: meta.created_ms,
                open: meta.open,
                preview,
            });
        }
        entries.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(b.id.cmp(&a.id)));
        Ok(entries)
    }
    /// Незавершённые сессии прежнего процесса остаются доступными, уведомление показывается один раз.
    pub fn recover(&self) -> io::Result<usize> {
        // Первичная запись, прерванная сбоем процесса, не показывается и не нужна.
        for entry in fs::read_dir(&self.root)?.take(1000).flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_prefix(".pending-")
                && self.dir(id).is_ok()
                && entry.file_type().is_ok_and(|t| t.is_dir())
            {
                let _ = remove_files(&entry.path());
            }
        }
        let mut count = 0;
        for entry in self.list()? {
            let dir = self.dir(&entry.id)?;
            // Прерванная замена не мешает читать предыдущий атомарно записанный вариант.
            for name in ["state.enc.part", "preview.enc.part", "meta.toml.part"] {
                let part = dir.join(name);
                if part.exists() && !fs::symlink_metadata(&part)?.file_type().is_symlink() {
                    fs::remove_file(part)?;
                }
            }
            if entry.open {
                let mut meta = read_meta(&dir)?;
                meta.open = false;
                write_meta(&dir, &meta)?;
                count += 1;
            }
        }
        Ok(count)
    }
    pub fn remove_if_revision(&self, id: &str, revision: u64) -> io::Result<bool> {
        let dir = self.dir(id)?;
        if !dir.exists() {
            return Ok(false);
        }
        if self.document(&dir)?.revision != revision {
            return Ok(false);
        }
        self.remove(id)?;
        Ok(true)
    }
    fn remove(&self, id: &str) -> io::Result<()> {
        remove_files(&self.dir(id)?)
    }
    pub fn clear(&self) -> io::Result<()> {
        for entry in self.list()? {
            self.remove(&entry.id)?;
        }
        Ok(())
    }
    pub fn set_max(&mut self, max: usize) -> io::Result<()> {
        self.max = max.clamp(1, 10);
        self.prune()
    }
    fn prune(&self) -> io::Result<()> {
        let mut entries = self.list()?;
        entries.sort_by(|a, b| {
            b.open
                .cmp(&a.open)
                .then(b.created_ms.cmp(&a.created_ms))
                .then(b.id.cmp(&a.id))
        });
        for entry in entries.into_iter().skip(self.max) {
            self.remove(&entry.id)?;
        }
        Ok(())
    }
}
pub fn from_array(r: [i32; 4]) -> Rect {
    Rect::new(r[0], r[1], r[2], r[3])
}
pub fn to_array(r: Rect) -> [i32; 4] {
    [r.left, r.top, r.right, r.bottom]
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
/// Никогда не удаляем каталог рекурсивно: чужие файлы/подкаталоги не затрагиваются.
fn remove_files(dir: &Path) -> io::Result<()> {
    for name in ["frames.enc", "state.enc", "preview.enc", "meta.toml"] {
        for suffix in ["", ".part"] {
            match fs::remove_file(dir.join(format!("{name}{suffix}"))) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    fs::remove_dir(dir)
}
fn read_limited(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("ссылка вместо файла"));
    }
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("слишком большой файл черновика"));
    }
    Ok(bytes)
}
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("ссылка вместо файла"));
    }
    let tmp = path.with_file_name(format!("{}.part", path.file_name().unwrap().to_string_lossy()));
    if tmp.exists() {
        return Err(invalid("остался незавершённый файл; исходный снимок не изменён"));
    }
    let mut file = fs::OpenOptions::new().create_new(true).write(true).open(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        // Иначе следующая запись этого файла упрётся в оставшийся .part до перезапуска.
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn write_meta(dir: &Path, meta: &Meta) -> io::Result<()> {
    write_atomic(
        &dir.join("meta.toml"),
        toml::to_string(meta).map_err(io::Error::other)?.as_bytes(),
    )
}
fn read_meta(dir: &Path) -> io::Result<Meta> {
    let bytes = read_limited(&dir.join("meta.toml"), 4096)?;
    let meta: Meta =
        toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?).map_err(io::Error::other)?;
    if meta.version != 1 {
        return Err(invalid("неподдерживаемая версия черновика"));
    }
    Ok(meta)
}
fn encode_frames(frames: &[Arc<Image>]) -> io::Result<Vec<u8>> {
    if frames.is_empty() || frames.len() > 8 {
        return Err(invalid("неверное число кадров"));
    }
    let mut out = b"KADRFR01".to_vec();
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    let mut total = 0;
    for frame in frames {
        if !pixels::valid_rect(frame.rect) {
            return Err(invalid("неверный размер кадра"));
        }
        total += frame.bgra.len() / 4;
        if total > MAX_TOTAL_PIXELS {
            return Err(invalid("слишком много пикселей"));
        }
        let png = pixels::encode_png(frame).map_err(io::Error::other)?;
        for n in to_array(frame.rect) {
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.extend_from_slice(&(png.len() as u64).to_le_bytes());
        out.extend_from_slice(&png);
        if out.len() as u64 > MAX_FILE {
            return Err(invalid("слишком большой набор кадров"));
        }
    }
    Ok(out)
}
fn decode_frames(mut bytes: &[u8]) -> io::Result<Vec<Arc<Image>>> {
    fn take<'a>(data: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
        if n > data.len() {
            return Err(invalid("неполный набор кадров"));
        }
        let (head, tail) = data.split_at(n);
        *data = tail;
        Ok(head)
    }
    if take(&mut bytes, 8)? != b"KADRFR01" {
        return Err(invalid("неверный заголовок кадров"));
    }
    let count = u32::from_le_bytes(take(&mut bytes, 4)?.try_into().unwrap());
    if !(1..=8).contains(&count) {
        return Err(invalid("неверное число кадров"));
    }
    let mut frames = Vec::new();
    let mut total = 0;
    for _ in 0..count {
        let mut rect = [0; 4];
        for n in &mut rect {
            *n = i32::from_le_bytes(take(&mut bytes, 4)?.try_into().unwrap());
        }
        let rect = from_array(rect);
        if !pixels::valid_rect(rect) {
            return Err(invalid("неверный размер кадра"));
        }
        total += rect.width() as usize * rect.height() as usize;
        if total > MAX_TOTAL_PIXELS {
            return Err(invalid("слишком много пикселей"));
        }
        let size = u64::from_le_bytes(take(&mut bytes, 8)?.try_into().unwrap());
        if size > MAX_FILE {
            return Err(invalid("слишком большой кадр"));
        }
        frames.push(Arc::new(pixels::decode_png(take(&mut bytes, size as usize)?, rect)?));
    }
    if !bytes.is_empty() {
        return Err(invalid("лишние данные в наборе кадров"));
    }
    Ok(frames)
}

#[cfg(test)]
mod tests;
