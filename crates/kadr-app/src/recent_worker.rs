//! Единственный писатель черновиков. PNG, DPAPI и диск не выполняются в UI-потоке.
use kadr_doc::Image;
use kadr_store::recent::{Crypto, Document, Draft, Entry, Store};
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Clone, Debug)]
pub struct Token {
    pub id: String,
    pub revision: u64,
}
pub enum Command {
    Create { token: Token, frames: Vec<Arc<Image>>, document: Document },
    State { token: Token, document: Document, open: bool },
    Remove(Token),
    Load(String),
    List,
    Clear,
    Pin { id: String, pinned: bool },
    Delete(String),
    Limit(usize),
    Shutdown,
}
pub enum Event {
    Ready { entries: Vec<Entry>, recovered: usize },
    List(Vec<Entry>),
    Loaded { id: String, draft: Draft },
    Persisted(Token),
    Error(String),
}
pub struct Dpapi;
impl Crypto for Dpapi {
    fn seal(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        kadr_win::protect::seal(data)
    }
    fn open(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        kadr_win::protect::open(data)
    }
}
pub struct Worker {
    tx: Sender<Command>,
    pub events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}
/// Контекст создаётся лениво на потоке хранилища и не пересекается с UI-контекстом D2D.
fn update_preview(
    store: &Store<Dpapi>,
    id: &str,
    doc: &Document,
    frames: &[Arc<Image>],
    renderer: &mut Option<(kadr_render::gpu::Shared, kadr_render::gpu::Gpu)>,
) -> io::Result<()> {
    if doc.annotations.is_empty() {
        return Ok(());
    }
    let render = (|| -> windows::core::Result<Image> {
        if renderer.is_none() {
            use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
            let shared = kadr_render::gpu::Shared::new()?;
            let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
            let gpu = kadr_render::gpu::Gpu::new(&shared, &unsafe { factory.EnumAdapters1(0)? }, "drafts".into())?;
            *renderer = Some((shared, gpu));
        }
        let (shared, gpu) = renderer.as_ref().unwrap();
        let bounds = kadr_doc::pixels::bounds(frames).unwrap();
        let area = doc.selection_rect().and_then(|r| r.intersect(&bounds)).unwrap_or(bounds);
        let image = kadr_render::annotations::flatten(gpu, shared, frames, area, &doc.annotations)?;
        Ok(kadr_doc::pixels::thumbnail(&[Arc::new(image)], area, 320, 160))
    })();
    // Документ уже записан; ошибка превью не должна препятствовать восстановлению снимка.
    match render {
        Ok(image) => store.set_preview(id, &image),
        Err(e) => {
            kadr_win::log_warn!("миниатюра M2: {e}");
            Ok(())
        }
    }
}
impl Worker {
    pub fn start(host: HWND, message: u32, max: usize, benchmark: bool) -> Self {
        let (tx, commands) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let host = host.0 as isize;
        let thread = std::thread::spawn(move || {
            if benchmark {
                return;
            }
            let emit = |event| {
                let _ = events_tx.send(event);
                unsafe {
                    let _ = PostMessageW(Some(HWND(host as *mut _)), message, WPARAM(0), LPARAM(0));
                }
            };
            let result = (|| -> io::Result<()> {
                let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| io::Error::other("LOCALAPPDATA не задан"))?;
                let mut store = Store::new(PathBuf::from(base).join("Kadr/recent"), Dpapi, max)?;
                let recovered = store.recover()?;
                store.set_max(max)?;
                emit(Event::Ready { entries: store.list()?, recovered });
                let mut frames: HashMap<String, Vec<Arc<Image>>> = HashMap::new();
                let mut renderer = None;
                while let Ok(command) = commands.recv() {
                    if matches!(command, Command::Shutdown) {
                        break;
                    }
                    let result = (|| -> io::Result<()> {
                        match command {
                            Command::Create { token, frames: captured, document } => {
                                store.create(&token.id, now_ms(), &captured, &document)?;
                                store.update(&token.id, &document, true, &captured)?;
                                frames.insert(token.id.clone(), captured);
                                emit(Event::Persisted(token));
                                emit(Event::List(store.list()?));
                            }
                            Command::State { token, document, open } => {
                                if let Some(captured) = frames.get(&token.id) {
                                    store.update(&token.id, &document, open, captured)?;
                                    update_preview(&store, &token.id, &document, captured, &mut renderer)?;
                                    emit(Event::Persisted(token.clone()));
                                    if !open {
                                        frames.remove(&token.id);
                                    }
                                }
                            }
                            Command::Remove(token) => {
                                store.remove_if_revision(&token.id, token.revision)?;
                                emit(Event::List(store.list()?));
                            }
                            Command::Load(id) => {
                                let mut draft = store.load(&id)?;
                                draft.document.revision += 1;
                                store.update(&id, &draft.document, true, &draft.frames)?;
                                update_preview(&store, &id, &draft.document, &draft.frames, &mut renderer)?;
                                frames.insert(id.clone(), draft.frames.clone());
                                emit(Event::Loaded { id, draft });
                            }
                            Command::List => emit(Event::List(store.list()?)),
                            Command::Clear => {
                                store.clear()?;
                                let retained = store.list()?;
                                frames.retain(|id, _| retained.iter().any(|e| &e.id == id));
                                emit(Event::List(store.list()?));
                            }
                            Command::Pin { id, pinned } => {
                                store.set_pinned(&id, pinned)?;
                                emit(Event::List(store.list()?));
                            }
                            Command::Delete(id) => {
                                store.remove(&id)?;
                                frames.remove(&id);
                                emit(Event::List(store.list()?));
                            }
                            Command::Limit(max) => {
                                store.set_max(max)?;
                                emit(Event::List(store.list()?));
                            }
                            Command::Shutdown => unreachable!(),
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        emit(Event::Error(error.to_string()));
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                emit(Event::Error(error.to_string()));
            }
        });
        Self { tx, events, thread: Some(thread) }
    }
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
