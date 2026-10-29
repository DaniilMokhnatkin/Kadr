//! Модель документа без Win32: геометрия, выделение, изображения. Координаты — физические пиксели
//! виртуального рабочего стола; правая и нижняя граница прямоугольника не включаются.

mod geometry;
mod image;

pub use geometry::{Handle, Point, Rect, drag_handle, hit_handle, window_at};
pub use image::{Image, hex};
pub mod annotation;
pub mod pixels;
pub mod privacy;
