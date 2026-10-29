//! Экспорт снимка: PNG, буфер обмена, файл. Всё, что уходит наружу, — плоская картинка.

pub mod clipboard;
pub mod file;

pub use kadr_doc::pixels::encode_png;

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_doc::{Image, Rect};

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
