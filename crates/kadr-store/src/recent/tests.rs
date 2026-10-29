use super::*;

// Только для проверки формата/жизненного цикла. Приложение использует системный DPAPI.
struct TestCrypto;
impl Crypto for TestCrypto {
    fn seal(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        Ok(data.iter().map(|b| b ^ 0xa5).collect())
    }
    fn open(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.seal(data)
    }
}
fn setup(name: &str) -> Store<TestCrypto> {
    let root = std::env::temp_dir().join(format!("kadr-recent-{name}-{}", std::process::id()));
    Store::new(root, TestCrypto, 10).unwrap()
}
fn frames() -> Vec<Arc<Image>> {
    vec![
        Arc::new(Image::new(Rect::new(-2, 0, 0, 2), vec![33; 16])),
        Arc::new(Image::new(Rect::new(0, 0, 2, 2), vec![71; 16])),
    ]
}
fn document() -> Document {
    Document {
        version: 1,
        revision: 1,
        selection: Some([-1, 0, 1, 2]),
        automatic: false,
        layout: vec![[-2, 0, 0, 2], [0, 0, 2, 2]],
    }
}
#[test]
fn captured_frames_and_selection_survive_reopening_the_store() {
    let store = setup("reopen");
    store.create("abc-1", 100, &frames(), &document()).unwrap();
    let root = store.root.clone();
    drop(store);
    let store = Store::new(root.clone(), TestCrypto, 10).unwrap();
    let draft = store.load("abc-1").unwrap();
    assert_eq!(draft.document, document());
    assert_eq!(draft.frames, frames());
    assert!(store.list().unwrap()[0].open);
    let raw = fs::read(root.join("abc-1/frames.enc")).unwrap();
    assert!(!raw.windows(8).any(|b| b == b"\x89PNG\r\n\x1a\n"));
    store.clear().unwrap();
    fs::remove_dir(root).unwrap();
}
#[test]
fn updates_do_not_rewrite_frames_and_stale_export_cannot_delete_new_edits() {
    let store = setup("revision");
    store.create("abc-2", 100, &frames(), &document()).unwrap();
    let original = fs::read(store.root.join("abc-2/frames.enc")).unwrap();
    let mut doc = document();
    doc.revision = 2;
    doc.selection = Some([0, 0, 1, 1]);
    store.update("abc-2", &doc, false, &frames()).unwrap();
    assert_eq!(fs::read(store.root.join("abc-2/frames.enc")).unwrap(), original);
    assert!(!store.remove_if_revision("abc-2", 1).unwrap());
    assert_eq!(store.load("abc-2").unwrap().document, doc);
    assert!(store.remove_if_revision("abc-2", 2).unwrap());
    assert!(store.list().unwrap().is_empty());
    fs::remove_dir(&store.root).unwrap();
}
#[test]
fn eleventh_draft_evicts_oldest_and_foreign_or_partial_directories_are_untouched() {
    let store = setup("retention");
    fs::create_dir_all(store.root.join("foreign-files")).unwrap();
    fs::write(store.root.join("foreign-files/important.txt"), "keep").unwrap();
    fs::create_dir_all(store.root.join(".pending-crash")).unwrap();
    for n in 0..11 {
        store.create(&format!("abc-{n:x}"), n, &frames(), &document()).unwrap();
    }
    let entries = store.list().unwrap();
    assert_eq!(entries.len(), 10);
    assert_eq!(entries[0].id, "abc-a");
    assert!(!store.root.join("abc-0").exists());
    store.clear().unwrap();
    assert!(store.root.join("foreign-files/important.txt").exists());
    // Контролируемый каталог теста; не путь, полученный из метаданных черновика.
    fs::remove_dir_all(&store.root).unwrap();
}
#[test]
fn traversal_corrupt_payload_and_oversized_images_are_rejected() {
    let store = setup("invalid");
    assert!(store.load("../other").is_err());
    assert!(store.create("../other", 1, &frames(), &document()).is_err());
    store.create("abc-f", 1, &frames(), &document()).unwrap();
    fs::write(store.root.join("abc-f/state.enc"), b"broken").unwrap();
    assert!(store.load("abc-f").is_err());
    assert!(kadr_doc::pixels::decode_png(b"not png", Rect::new(0, 0, i32::MAX, 2)).is_err());
    store.clear().unwrap();
    fs::remove_dir(&store.root).unwrap();
}

#[test]
fn interrupted_state_replacement_keeps_previous_document_and_recovers_once() {
    let store = setup("interrupted");
    store.create("abc-3", 100, &frames(), &document()).unwrap();
    fs::write(store.root.join("abc-3/state.enc.part"), b"half a new document").unwrap();
    assert_eq!(store.load("abc-3").unwrap().document, document());
    assert_eq!(store.recover().unwrap(), 1);
    assert_eq!(store.recover().unwrap(), 0);
    let mut doc = document();
    doc.revision = 2;
    store.update("abc-3", &doc, false, &frames()).unwrap();
    assert_eq!(store.load("abc-3").unwrap().document.revision, 2);
    store.clear().unwrap();
    fs::remove_dir(&store.root).unwrap();
}

#[test]
fn abrupt_process_exit_leaves_recoverable_open_draft() {
    const CHILD: &str = "KADR_RECENT_CRASH_FIXTURE";
    if let Some(root) = std::env::var_os(CHILD) {
        let store = Store::new(PathBuf::from(root), TestCrypto, 10).unwrap();
        store.create("abc-c", 100, &frames(), &document()).unwrap();
        let mut doc = document();
        doc.revision = 2;
        doc.selection = Some([0, 0, 1, 2]);
        store.update("abc-c", &doc, true, &frames()).unwrap();
        // Выход обходит деструкторы и любое штатное закрытие сессии.
        std::process::exit(17);
    }
    let store = setup("process-exit");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "recent::tests::abrupt_process_exit_leaves_recoverable_open_draft"])
        .env(CHILD, &store.root)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(17));
    assert_eq!(store.recover().unwrap(), 1);
    let draft = store.load("abc-c").unwrap();
    assert_eq!(draft.document.selection, Some([0, 0, 1, 2]));
    assert_eq!(draft.frames, frames());
    store.clear().unwrap();
    fs::remove_dir(&store.root).unwrap();
}

#[test]
fn failed_writes_leave_no_partial_files_behind() {
    let store = setup("failed-write");
    // Пустой набор кадров отклоняется уже после создания .pending-каталога.
    assert!(store.create("abc-4", 1, &[], &document()).is_err());
    assert!(!store.root.join(".pending-abc-4").exists());
    // Каталог на месте файла: rename падает, а .part не должен блокировать следующую запись.
    let target = store.root.join("blocked.enc");
    fs::create_dir(&target).unwrap();
    assert!(write_atomic(&target, b"data").is_err());
    assert!(!store.root.join("blocked.enc.part").exists());
    fs::remove_dir(&target).unwrap();
    fs::remove_dir(&store.root).unwrap();
}

#[test]
fn recover_removes_pending_writes_of_a_crashed_process_but_not_foreign_files() {
    let store = setup("pending");
    fs::create_dir(store.root.join(".pending-abc-5")).unwrap();
    fs::write(store.root.join(".pending-abc-5/frames.enc"), b"half").unwrap();
    fs::create_dir(store.root.join(".pending-abc-6")).unwrap();
    fs::write(store.root.join(".pending-abc-6/notes.txt"), b"keep").unwrap();
    assert_eq!(store.recover().unwrap(), 0);
    assert!(!store.root.join(".pending-abc-5").exists());
    assert!(store.root.join(".pending-abc-6/notes.txt").exists());
    fs::remove_dir_all(&store.root).unwrap();
}
