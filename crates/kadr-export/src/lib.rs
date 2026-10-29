//! Экспорт снимка: PNG, буфер обмена, файл. Всё, что уходит наружу, — плоская картинка.

pub mod clipboard;
pub mod file;

use kadr_doc::Image;

/// PNG из BGRA-изображения. Быстрое сжатие: скриншоты хорошо жмутся и так, а ждать никто не должен.
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

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_doc::Rect;

    #[test]
    fn png_round_trip_keeps_pixels() {
        let bgra = vec![10, 20, 30, 255, 40, 50, 60, 0];
        let image = Image::new(Rect::new(5, 5, 7, 6), bgra);
        let bytes = encode_png(&image).unwrap();

        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&buf[..info.buffer_size()], &[30, 20, 10, 255, 60, 50, 40, 0]);
    }
}
