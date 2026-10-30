use super::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
unsafe fn send(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
    unsafe {
        let _ = SendMessageW(hwnd, msg, Some(wp), Some(lp));
    }
}
fn image() -> Arc<Image> {
    Arc::new(Image::new(kadr_doc::Rect::new(0, 0, 640, 360), vec![180; 640 * 360 * 4]))
}
#[test]
fn hidden_native_window_accepts_commands_and_releases_state_without_touching_focus() {
    unsafe {
        let before = GetForegroundWindow();
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let source = image();
        let window = Window::new(instance, HWND::default(), WM_APP + 88, source.clone(), HICON::default()).unwrap();
        assert!(!IsWindowVisible(window.hwnd).as_bool());
        assert_eq!(GetForegroundWindow(), before);
        let minus = GetDlgItem(Some(window.hwnd), MINUS as i32).unwrap();
        let mut name = [0u16; 80];
        let count = GetWindowTextW(minus, &mut name);
        assert_eq!(String::from_utf16_lossy(&name[..count as usize]), "Уменьшить масштаб");
        let original = data(window.hwnd).unwrap().borrow().pin.zoom;
        send(window.hwnd, WM_COMMAND, WPARAM(MINUS), LPARAM(0));
        assert!(data(window.hwnd).unwrap().borrow().pin.zoom < original);
        send(window.hwnd, WM_MOUSEWHEEL, WPARAM(8 | (((120u32) as usize) << 16)), LPARAM(0));
        assert_eq!(data(window.hwnd).unwrap().borrow().pin.opacity, 255);
        send(window.hwnd, WM_MOUSEWHEEL, WPARAM(8 | (((-120i16 as u16) as usize) << 16)), LPARAM(0));
        assert!(data(window.hwnd).unwrap().borrow().pin.opacity < 255);
        send(window.hwnd, WM_COMMAND, WPARAM(COPY), LPARAM(0));
        assert!(matches!(requests().as_slice(),[Request::Copy(i)] if i.as_ref()==source.as_ref()));
        assert_eq!(GetForegroundWindow(), before);
        send(window.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        assert!(!window.alive());
        assert!(data(window.hwnd).is_none());
        drop(window);
        assert_eq!(GetForegroundWindow(), before);
    }
}
#[test]
fn pin_controls_fit_and_render_at_three_dpi_scales() {
    for scale in [1.0, 1.5, 2.0] {
        let preview = preview(&image(), scale).unwrap();
        let r = RECT { left: 0, top: 0, right: preview.width() as i32, bottom: preview.height() as i32 };
        let buttons = paint::buttons(r, scale);
        assert_eq!(buttons.len(), 6);
        for b in &buttons {
            assert!(b.left >= 0 && b.right <= r.right && b.top >= 0 && b.bottom <= r.bottom);
            assert!(b.bottom - b.top >= 36);
        }
        assert!(buttons.windows(2).all(|p| p[0].right < p[1].left));
        assert!(preview.bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        if let Ok(dir) = std::env::var("KADR_TEST_ARTIFACTS") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                format!("{dir}/pin-{}.png", (scale * 100.0) as i32),
                kadr_doc::pixels::encode_png(&preview).unwrap(),
            )
            .unwrap();
        }
    }
}
