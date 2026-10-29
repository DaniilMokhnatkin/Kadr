//! PNG и ограниченные преобразования кадров для восстановления сессий.
use crate::{Image, Point, Rect};
use std::io::{self, Cursor};
use std::sync::Arc;

pub const MAX_PIXELS: usize = 32 * 1024 * 1024;
pub fn valid_rect(r: Rect) -> bool {
    [r.left, r.top, r.right, r.bottom].iter().all(|v| (-65536..=65536).contains(v))
        && r.width() > 0
        && r.height() > 0
        && (r.width() as usize).checked_mul(r.height() as usize).is_some_and(|n| n <= MAX_PIXELS)
}
pub fn encode_png(image: &Image) -> Result<Vec<u8>, png::EncodingError> {
    let mut rgba = Vec::with_capacity(image.bgra.len());
    for [b, g, r, a] in image.bgra.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[*r, *g, *b, *a]);
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(out)
}
pub fn decode_png(bytes: &[u8], rect: Rect) -> io::Result<Image> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "недопустимый размер или формат кадра");
    if !valid_rect(rect) {
        return Err(invalid());
    }
    let decoder = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: MAX_PIXELS * 4 });
    let mut reader = decoder.read_info().map_err(io::Error::other)?;
    let info = reader.info();
    if info.width != rect.width() as u32
        || info.height != rect.height() as u32
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
    {
        return Err(invalid());
    }
    let size = reader.output_buffer_size().ok_or_else(invalid)?;
    if size > MAX_PIXELS * 4 {
        return Err(invalid());
    }
    let mut pixels = vec![0; size];
    reader.next_frame(&mut pixels).map_err(io::Error::other)?;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Image::new(rect, pixels))
}
pub fn bounds(frames: &[Arc<Image>]) -> Option<Rect> {
    frames.iter().map(|f| f.rect).reduce(|a, b| a.union(&b))
}
/// Уменьшает без создания полноразмерной копии объединённого рабочего стола.
pub fn thumbnail(frames: &[Arc<Image>], area: Rect, max_w: u32, max_h: u32) -> Image {
    let scale = (max_w as f64 / area.width() as f64).min(max_h as f64 / area.height() as f64).min(1.0);
    let w = (area.width() as f64 * scale).round().max(1.0) as i32;
    let h = (area.height() as f64 * scale).round().max(1.0) as i32;
    let mut pixels = vec![0; w as usize * h as usize * 4];
    for y in 0..h {
        for x in 0..w {
            let p = Point::new(
                area.left + ((x as f64 / scale) as i32).min(area.width() - 1),
                area.top + ((y as f64 / scale) as i32).min(area.height() - 1),
            );
            if let Some(mut pixel) = frames.iter().find_map(|f| f.pixel(p)) {
                pixel[3] = 255;
                let offset = ((y * w + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&pixel);
            }
        }
    }
    Image::new(Rect::new(0, 0, w, h), pixels)
}
/// При другой раскладке старый рабочий стол целиком вписывается в основной монитор.
pub fn fit_to_monitor(frames: &[Arc<Image>], selection: Option<Rect>, monitor: Rect) -> (Image, Option<Rect>) {
    let area = bounds(frames).unwrap();
    let mut image = thumbnail(frames, area, monitor.width() as u32, monitor.height() as u32);
    let x = monitor.left + (monitor.width() - image.rect.width()) / 2;
    let y = monitor.top + (monitor.height() - image.rect.height()) / 2;
    image.rect = image.rect.offset(x, y);
    let sx = image.width() as f64 / area.width() as f64;
    let sy = image.height() as f64 / area.height() as f64;
    let selection = selection.and_then(|r| r.intersect(&area)).map(|r| {
        Rect::new(
            x + ((r.left - area.left) as f64 * sx).floor() as i32,
            y + ((r.top - area.top) as f64 * sy).floor() as i32,
            x + ((r.right - area.left) as f64 * sx).ceil() as i32,
            y + ((r.bottom - area.top) as f64 * sy).ceil() as i32,
        )
    });
    (image, selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn png_roundtrip_checks_declared_dimensions_and_keeps_pixels() {
        let image = Image::new(Rect::new(-2, 0, 0, 1), vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let png = encode_png(&image).unwrap();
        assert_eq!(decode_png(&png, image.rect).unwrap(), image);
        assert!(decode_png(&png, Rect::new(0, 0, 4, 1)).is_err());
    }
    #[test]
    fn changed_layout_fits_negative_coordinates_and_maps_selection() {
        let source = Arc::new(Image::new(Rect::new(-4, 0, 4, 4), vec![77; 8 * 4 * 4]));
        let (image, selection) = fit_to_monitor(&[source], Some(Rect::new(-4, 0, 0, 4)), Rect::new(0, 0, 4, 4));
        assert_eq!(image.rect, Rect::new(0, 1, 4, 3));
        assert_eq!(selection, Some(Rect::new(0, 1, 2, 3)));
    }
}
