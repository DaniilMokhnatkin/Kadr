//! QR codes in the selection: Ctrl+Shift+C reads them before looking for text, since a code
//! carries exactly what its author meant (often a link), and OCR would only see noise.
use kadr_doc::Image;

/// The first readable code's contents.
pub fn decode(image: &Image) -> Option<String> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w < 21 || h < 21 {
        return None;
    }
    // Small codes on a high-DPI screen read better at twice the size; big ones as they are.
    let k = if w.max(h) < 400 { 2 } else { 1 };
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w * k, h * k, |x, y| {
        let i = ((y / k) * w + x / k) * 4;
        let (b, g, r) = (image.bgra[i] as u32, image.bgra[i + 1] as u32, image.bgra[i + 2] as u32);
        ((r * 299 + g * 587 + b * 114) / 1000) as u8
    });
    prepared.detect_grids().iter().find_map(|grid| grid.decode().ok()).map(|(_, text)| text).filter(|t| !t.is_empty())
}

/// Only web links are offered to open; anything else is just copied.
pub fn link(text: &str) -> Option<&str> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    ((lower.starts_with("https://") || lower.starts_with("http://")) && !t.chars().any(char::is_whitespace))
        .then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_doc::Rect;

    fn render(text: &str, module: usize) -> Image {
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let n = code.width();
        let quiet = 4;
        let side = (n + quiet * 2) * module;
        let mut bgra = vec![255u8; side * side * 4];
        for y in 0..n {
            for x in 0..n {
                if code[(x, y)] == qrcode::Color::Dark {
                    for dy in 0..module {
                        for dx in 0..module {
                            let i = (((y + quiet) * module + dy) * side + (x + quiet) * module + dx) * 4;
                            bgra[i..i + 3].fill(0);
                        }
                    }
                }
            }
        }
        Image::new(Rect::new(0, 0, side as i32, side as i32), bgra)
    }

    #[test]
    fn reads_a_code_at_screen_sizes() {
        for module in [2, 3, 6] {
            let image = render("https://github.com/kadr?a=1&b=два", module);
            assert_eq!(decode(&image).as_deref(), Some("https://github.com/kadr?a=1&b=два"), "module {module}");
        }
        assert_eq!(decode(&Image::new(Rect::new(0, 0, 64, 64), vec![255; 64 * 64 * 4])), None);
    }

    #[test]
    fn only_web_links_are_opened() {
        assert_eq!(link(" https://example.com/x "), Some("https://example.com/x"));
        assert_eq!(link("HTTP://EXAMPLE.COM"), Some("HTTP://EXAMPLE.COM"));
        assert_eq!(link("WIFI:S:home;T:WPA;P:secret;;"), None);
        assert_eq!(link("file:///C:/Windows"), None);
        assert_eq!(link("https://a.b c"), None);
    }
}
