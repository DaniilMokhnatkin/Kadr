//! Clipboard history: what was copied, newest first, kept across restarts. Each entry is its own
//! file sealed with the same per-user encryption as the drafts, so a crash loses at most the
//! entry being written and nothing is readable by another account. Pinned entries never age out.
use crate::recent::Crypto;
use std::fs;
use std::io;
use std::path::PathBuf;

const MAGIC: &[u8; 4] = b"KCL1";
/// A single entry may not be larger than this on disk (a big screenshot as PNG fits easily).
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// Pictures are heavy: of the unpinned entries, only this many keep theirs.
const MAX_IMAGES: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    Text(String),
    /// A picture as PNG, with its size for the list.
    Image {
        width: u32,
        height: u32,
        png: Vec<u8>,
    },
    /// Files copied in Explorer: their full paths.
    Files(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub id: u64,
    pub created_ms: u64,
    pub pinned: bool,
    pub data: Data,
}

pub struct Clips<C: Crypto> {
    root: PathBuf,
    crypto: C,
    /// Unpinned entries kept; pinned ones come on top of that.
    max: usize,
    /// Newest first.
    items: Vec<Clip>,
    next_id: u64,
}

impl<C: Crypto> Clips<C> {
    /// Opens the folder and reads what is there; unreadable entries are skipped and removed.
    pub fn open(root: PathBuf, crypto: C, max: usize) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut items = Vec::new();
        for entry in fs::read_dir(&root)?.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "clip") {
                continue;
            }
            let read = || -> io::Result<Clip> {
                if entry.metadata()?.len() > MAX_FILE {
                    return Err(io::Error::other("слишком большой"));
                }
                decode(&crypto.open(&fs::read(&path)?)?).ok_or_else(|| io::Error::other("повреждён"))
            };
            match read() {
                Ok(clip) => items.push(clip),
                Err(_) => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        items.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(b.id.cmp(&a.id)));
        let next_id = items.iter().map(|c| c.id + 1).max().unwrap_or(1);
        let mut clips = Self { root, crypto, max: max.max(1), items, next_id };
        clips.prune()?;
        Ok(clips)
    }

    pub fn items(&self) -> &[Clip] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&Clip> {
        self.items.iter().find(|c| c.id == id)
    }

    /// Something was copied. The same content again only moves its entry to the top.
    /// Returns the entry's id.
    pub fn add(&mut self, data: Data, now_ms: u64) -> io::Result<u64> {
        if let Some(i) = self.items.iter().position(|c| c.data == data) {
            let mut clip = self.items.remove(i);
            clip.created_ms = now_ms.max(clip.created_ms);
            self.write(&clip)?;
            let id = clip.id;
            self.items.insert(0, clip);
            return Ok(id);
        }
        let clip = Clip { id: self.next_id, created_ms: now_ms, pinned: false, data };
        self.next_id += 1;
        self.write(&clip)?;
        let id = clip.id;
        self.items.insert(0, clip);
        self.prune()?;
        Ok(id)
    }

    /// Pasted from the history: it becomes the newest again.
    pub fn touch(&mut self, id: u64, now_ms: u64) -> io::Result<()> {
        let Some(i) = self.items.iter().position(|c| c.id == id) else { return Ok(()) };
        let mut clip = self.items.remove(i);
        clip.created_ms = now_ms.max(clip.created_ms);
        self.write(&clip)?;
        self.items.insert(0, clip);
        Ok(())
    }

    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> io::Result<()> {
        let Some(clip) = self.items.iter_mut().find(|c| c.id == id) else { return Ok(()) };
        clip.pinned = pinned;
        let clip = clip.clone();
        self.write(&clip)?;
        self.prune()
    }

    pub fn remove(&mut self, id: u64) -> io::Result<()> {
        self.items.retain(|c| c.id != id);
        remove_file(&self.path(id))
    }

    /// «Очистить»: everything except what is pinned.
    pub fn clear(&mut self) -> io::Result<()> {
        let gone: Vec<u64> = self.items.iter().filter(|c| !c.pinned).map(|c| c.id).collect();
        self.items.retain(|c| c.pinned);
        for id in gone {
            remove_file(&self.path(id))?;
        }
        Ok(())
    }

    pub fn set_max(&mut self, max: usize) -> io::Result<()> {
        self.max = max.max(1);
        self.prune()
    }

    fn path(&self, id: u64) -> PathBuf {
        self.root.join(format!("{id:016x}.clip"))
    }

    /// Atomic: a half-written entry is never read back.
    fn write(&self, clip: &Clip) -> io::Result<()> {
        let path = self.path(clip.id);
        let part = path.with_extension("part");
        fs::write(&part, self.crypto.seal(&encode(clip))?)?;
        fs::rename(&part, &path).inspect_err(|_| {
            let _ = fs::remove_file(&part);
        })
    }

    /// The oldest unpinned entries go once there are too many, pictures sooner than text.
    fn prune(&mut self) -> io::Result<()> {
        let (mut kept, mut images) = (0, 0);
        let mut gone = Vec::new();
        self.items.retain(|c| {
            if c.pinned {
                return true;
            }
            let image = matches!(c.data, Data::Image { .. });
            let keep = kept < self.max && (!image || images < MAX_IMAGES);
            if keep {
                kept += 1;
                images += usize::from(image);
            } else {
                gone.push(c.id);
            }
            keep
        });
        for id in gone {
            remove_file(&self.path(id))?;
        }
        Ok(())
    }
}

fn remove_file(path: &std::path::Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn encode(clip: &Clip) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    let (kind, payload): (u8, Vec<u8>) = match &clip.data {
        Data::Text(text) => (1, text.as_bytes().to_vec()),
        Data::Image { width, height, png } => {
            let mut p = width.to_le_bytes().to_vec();
            p.extend(height.to_le_bytes());
            p.extend(png);
            (2, p)
        }
        Data::Files(paths) => (3, paths.join("\n").into_bytes()),
    };
    out.push(kind);
    out.push(u8::from(clip.pinned));
    out.extend(clip.id.to_le_bytes());
    out.extend(clip.created_ms.to_le_bytes());
    out.extend(payload);
    out
}

fn decode(bytes: &[u8]) -> Option<Clip> {
    let rest = bytes.strip_prefix(MAGIC)?;
    let (&kind, rest) = rest.split_first()?;
    let (&pinned, rest) = rest.split_first()?;
    let num = |b: &[u8]| -> Option<u64> { Some(u64::from_le_bytes(b.get(..8)?.try_into().ok()?)) };
    let (id, created_ms) = (num(rest)?, num(rest.get(8..)?)?);
    let payload = rest.get(16..)?;
    let data = match kind {
        1 => Data::Text(String::from_utf8(payload.to_vec()).ok()?),
        2 => {
            let width = u32::from_le_bytes(payload.get(..4)?.try_into().ok()?);
            let height = u32::from_le_bytes(payload.get(4..8)?.try_into().ok()?);
            Data::Image { width, height, png: payload.get(8..)?.to_vec() }
        }
        3 => Data::Files(String::from_utf8(payload.to_vec()).ok()?.split('\n').map(String::from).collect()),
        _ => return None,
    };
    Some(Clip { id, created_ms, pinned: pinned != 0, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCrypto;
    impl Crypto for TestCrypto {
        fn seal(&self, data: &[u8]) -> io::Result<Vec<u8>> {
            Ok(data.iter().map(|b| b ^ 0x5a).collect())
        }
        fn open(&self, data: &[u8]) -> io::Result<Vec<u8>> {
            self.seal(data)
        }
    }
    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("kadr-clips-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }
    fn text(s: &str) -> Data {
        Data::Text(s.into())
    }

    #[test]
    fn entries_survive_a_restart_newest_first_with_their_pins() {
        let root = root("restart");
        let mut clips = Clips::open(root.clone(), TestCrypto, 50).unwrap();
        let a = clips.add(text("первый"), 10).unwrap();
        clips.add(Data::Image { width: 2, height: 1, png: vec![1, 2, 3] }, 20).unwrap();
        clips.add(Data::Files(vec![r"C:\a.txt".into(), r"D:\b c.png".into()]), 30).unwrap();
        clips.set_pinned(a, true).unwrap();
        let before = clips.items().to_vec();
        drop(clips);
        let again = Clips::open(root.clone(), TestCrypto, 50).unwrap();
        assert_eq!(again.items(), before.as_slice());
        assert!(again.get(a).unwrap().pinned);
        // On disk nothing is readable as is.
        let raw: Vec<u8> = fs::read_dir(&root).unwrap().flat_map(|e| fs::read(e.unwrap().path()).unwrap()).collect();
        assert!(!raw.windows(MAGIC.len()).any(|w| w == MAGIC));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_same_text_again_moves_up_instead_of_repeating() {
        let root = root("dedupe");
        let mut clips = Clips::open(root.clone(), TestCrypto, 50).unwrap();
        let a = clips.add(text("a"), 1).unwrap();
        clips.add(text("b"), 2).unwrap();
        assert_eq!(clips.add(text("a"), 3).unwrap(), a);
        assert_eq!(clips.items().iter().map(|c| c.id).collect::<Vec<_>>(), [a, 2]);
        clips.touch(2, 4).unwrap();
        assert_eq!(clips.items()[0].id, 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn old_entries_age_out_but_pinned_and_cleared_behave() {
        let root = root("prune");
        let mut clips = Clips::open(root.clone(), TestCrypto, 3).unwrap();
        let first = clips.add(text("0"), 0).unwrap();
        clips.set_pinned(first, true).unwrap();
        for i in 1..=5 {
            clips.add(text(&i.to_string()), i).unwrap();
        }
        let left: Vec<String> = clips
            .items()
            .iter()
            .map(|c| match &c.data {
                Data::Text(t) => t.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(left, ["5", "4", "3", "0"], "three newest plus the pinned one");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 4);
        clips.clear().unwrap();
        assert_eq!(clips.items().len(), 1);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        clips.remove(first).unwrap();
        assert!(clips.items().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pictures_age_out_sooner_than_text() {
        let root = root("images");
        let mut clips = Clips::open(root.clone(), TestCrypto, 50).unwrap();
        for i in 0..(MAX_IMAGES as u64 + 3) {
            clips.add(Data::Image { width: 1, height: 1, png: i.to_le_bytes().to_vec() }, i).unwrap();
        }
        clips.add(text("t"), 100).unwrap();
        let images = clips.items().iter().filter(|c| matches!(c.data, Data::Image { .. })).count();
        assert_eq!(images, MAX_IMAGES);
        assert_eq!(clips.items().len(), MAX_IMAGES + 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_file_is_dropped_not_fatal() {
        let root = root("broken");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("0000000000000001.clip"), b"garbage").unwrap();
        let clips = Clips::open(root.clone(), TestCrypto, 50).unwrap();
        assert!(clips.items().is_empty());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
