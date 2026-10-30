//! Интеграционный сценарий без HWND, SendInput, захвата экрана и системного буфера обмена.
//! Использует реальный контроллер, Direct2D, PNG и DPAPI, но только синтетический кадр.
use kadr_doc::{
    Image, Point, Rect,
    annotation::{Annotation, Tool},
    pixels,
};
use kadr_editor::drawing::Drawing;
use kadr_render::{
    annotations,
    gpu::{Gpu, Shared},
};
use kadr_store::recent::{Document, Store, to_array};
use std::sync::Arc;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

#[test]
fn draw_export_encrypt_restore_and_export_again_without_desktop_input() {
    let root = std::env::temp_dir().join(format!("kadr-m2-flow-{}", std::process::id()));
    let store = Store::new(root.clone(), crate::recent_worker::Dpapi, 10).unwrap();
    let crop = Rect::new(-100, 0, 700, 500);
    let mut pixels = Vec::new();
    for y in 0..500 {
        for x in 0..800 {
            let c = if (x / 20 + y / 20) % 2 == 0 { 230 } else { 245 };
            pixels.extend_from_slice(&[c, c, c, 255]);
        }
    }
    let frames = vec![Arc::new(Image::new(crop, pixels))];
    let mut drawing = Drawing::default();
    for (i, tool) in Tool::ALL.into_iter().filter(|t| *t != Tool::Select).enumerate() {
        let p = Point::new(-60 + (i % 5) as i32 * 150, 60 + (i / 5) as i32 * 140);
        drawing.set_tool(tool);
        assert!(drawing.down(p, crop, 42 + i as u64));
        if tool == Tool::Text {
            drawing.text_insert("Привет 🌍");
            drawing.text_key(0x41, false, true);
            drawing.text_insert("Тест M2");
            drawing.finish_text();
        } else {
            drawing.motion(Point::new(p.x + 25, p.y + 50), crop, false);
            drawing.up(Point::new(p.x + 95, p.y + 70), crop, false);
        }
    }
    let committed = drawing.shapes().to_vec();
    drawing.undo();
    assert_eq!(drawing.shapes().len(), 12);
    drawing.redo();
    assert_eq!(drawing.shapes(), committed);
    drawing.set_tool(Tool::Select);
    drawing.down(Point::new(280, 375), crop, 99);
    drawing.up(Point::new(290, 380), crop, false);
    drawing.undo();
    assert_eq!(drawing.shapes(), committed);
    let shared = Shared::new().unwrap();
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
    let gpu = Gpu::new(&shared, &unsafe { factory.EnumAdapters1(0).unwrap() }, "m2-integration".into()).unwrap();
    let result = annotations::flatten(&gpu, &shared, &frames, crop, drawing.shapes()).unwrap();
    let encoded = pixels::encode_png(&result).unwrap();
    assert_eq!(pixels::decode_png(&encoded, crop).unwrap(), result);
    let doc = Document {
        version: 1,
        revision: 1,
        selection: Some(to_array(crop)),
        automatic: false,
        layout: vec![to_array(crop)],
        annotations: drawing.shapes().to_vec(),
    };
    store.create("ab-22", 100, &frames, &doc).unwrap();
    store.update("ab-22", &doc, false, &frames).unwrap();
    let thumb = pixels::thumbnail(&[Arc::new(result.clone())], crop, 320, 160);
    store.set_preview("ab-22", &thumb).unwrap();
    assert_eq!(store.list().unwrap()[0].preview.as_ref().unwrap(), &thumb);
    drop(store);
    let store = Store::new(root.clone(), crate::recent_worker::Dpapi, 10).unwrap();
    let draft = store.load("ab-22").unwrap();
    let mut restored = Drawing::default();
    restored.restore(draft.document.annotations);
    assert!(!restored.can_undo());
    let again = annotations::flatten(&gpu, &shared, &draft.frames, crop, restored.shapes()).unwrap();
    assert!(again == result);
    let encrypted = std::fs::read(root.join("ab-22/state.enc")).unwrap();
    let plaintext = "Тест M2".as_bytes();
    assert!(!encrypted.windows(plaintext.len()).any(|s| s == plaintext));
    if let Ok(dir) = std::env::var("KADR_TEST_ARTIFACTS") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(format!("{dir}/m2-flow.png"), encoded).unwrap();
    }
    store.clear().unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn keyboard_edit_delete_and_resize_are_undoable() {
    let crop = Rect::new(0, 0, 800, 600);
    let mut drawing = Drawing::default();
    drawing.set_tool(Tool::Rectangle);
    drawing.down(Point::new(100, 100), crop, 1);
    drawing.up(Point::new(200, 200), crop, false);
    let initial: Annotation = drawing.shapes()[0].clone();
    drawing.set_tool(Tool::Select);
    drawing.down(Point::new(100, 150), crop, 1);
    drawing.up(Point::new(100, 150), crop, false);
    drawing.nudge(10, 5, crop);
    assert_ne!(drawing.shapes()[0], initial);
    drawing.undo();
    assert_eq!(drawing.shapes()[0], initial);
    drawing.set_tool(Tool::Select);
    drawing.down(Point::new(100, 150), crop, 1);
    drawing.up(Point::new(100, 150), crop, false);
    let bounds = drawing.selected_bounds().unwrap();
    let corner = Point::new(bounds.right, bounds.bottom);
    drawing.down(corner, crop, 1);
    drawing.up(Point::new(corner.x + 70, corner.y + 60), crop, false);
    assert!(drawing.shapes()[0].rect().width() > initial.rect().width());
    drawing.undo();
    assert_eq!(drawing.shapes()[0], initial);
    drawing.set_tool(Tool::Select);
    drawing.down(Point::new(100, 150), crop, 1);
    drawing.up(Point::new(100, 150), crop, false);
    drawing.delete();
    assert!(drawing.shapes().is_empty());
    drawing.undo();
    assert_eq!(drawing.shapes()[0], initial);
}
