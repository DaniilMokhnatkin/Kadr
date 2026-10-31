//! Translation in place: a window exactly over the selection shows the frozen picture, and each
//! foreign block is painted over with its translation in the colours of the original, the way a
//! phone camera translates a sign. Hover lights a block up and a click copies its text; the pill
//! next to it has «Оригинал», «Копировать всё» and ×. Esc, × or a click elsewhere closes it.
//! Not hidden from capture on purpose: PrtSc over it takes the translated picture.
use crate::glass::{self, Device, Surface, fade, fill, inflate, inside, mix, rect, shadow, stroke, text};
use kadr_doc::{Image, Point, Rect};
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRectRgn, DeleteObject, EndPaint, GetMonitorInfoW, HGDIOBJ, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, RGN_OR, SetWindowRgn,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_ESCAPE, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, w};
use windows_numerics::Vector2;

const WM_MOUSELEAVE: u32 = 0x02A3;
const PILL_H: f32 = 40.0;
/// Room around the pill for its shadow.
const HALO: f32 = 14.0;
/// How long «Скопировано» stays over a block, in seconds.
const FLASH: f64 = 1.3;

/// A block of recognised text and what goes over it.
#[derive(Clone, Debug)]
pub struct Block {
    /// Desktop pixels.
    pub rect: Rect,
    pub lines: usize,
    /// Height of one line's letters and the distance between lines, in pixels.
    pub line_height: f32,
    pub pitch: f32,
    pub original: String,
    /// `None`: it stays as it is (already in the reader's language, or a name).
    pub translated: Option<String>,
}

#[derive(Clone)]
pub enum Content {
    Working,
    Ready { blocks: Vec<Block>, from: String, to: String },
    Failed(String),
}

/// Where the translation sits inside the picture, laid out once per result.
struct Fit {
    layout: IDWriteTextLayout,
    /// The patch that hides the original, in picture pixels.
    patch: D2D_RECT_F,
    at: (f32, f32),
    /// The font size used, and the one that matches the original letters.
    size: f32,
    start: f32,
}

struct Shown {
    block: Block,
    background: D2D1_COLOR_F,
    /// The backdrop at two opposite sides, and whether they are left and right (else top and bottom).
    sides: (D2D1_COLOR_F, D2D1_COLOR_F, bool),
    /// The patch's gradient between `sides`; made with the device.
    brush: Option<ID2D1LinearGradientBrush>,
    ink: D2D1_COLOR_F,
    bold: bool,
    appear: Spring,
    hover: Spring,
    fit: Option<Fit>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Block(usize),
    Original,
    CopyAll,
    Close,
}

struct Fonts {
    label: IDWriteTextFormat,
    button: IDWriteTextFormat,
}

struct State {
    hwnd: HWND,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<(f32, Fonts)>,
    image: Option<Arc<Image>>,
    bitmap: Option<ID2D1Bitmap1>,
    /// Working, the languages, or the error, without the blocks.
    content: Content,
    blocks: Vec<Shown>,
    laid_out: bool,
    scale: f32,
    epoch: Instant,
    appear: Spring,
    /// 1 shows the original under the patches.
    original: Spring,
    buttons: [Spring; 3],
    hover: Option<Hit>,
    tracking: bool,
    /// Which block was copied (`None`: all of it) and when.
    flash: Option<(Option<usize>, f64)>,
    /// The window's top-left on the desktop and the picture's offset inside it.
    origin: (i32, i32),
    picture_at: (f32, f32),
    pill: D2D_RECT_F,
    size: (f32, f32),
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

// ---- colours -------------------------------------------------------------------------------

fn luminance(c: [u8; 3]) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

/// The background around a block (median of a ring just outside it), the ink of its letters (the
/// pixels inside that differ from it most) and whether the letters look bold. RGB.
fn colours(image: &Image, r: Rect) -> ([u8; 3], [u8; 3], bool) {
    let rgb = |p: [u8; 4]| [p[2], p[1], p[0]];
    let mut ring = Vec::new();
    for pad in [2, 3] {
        for x in r.left - pad..r.right + pad {
            for y in [r.top - pad, r.bottom + pad - 1] {
                ring.extend(image.pixel(Point::new(x, y)).map(rgb));
            }
        }
        for y in r.top - pad..r.bottom + pad {
            for x in [r.left - pad, r.right + pad - 1] {
                ring.extend(image.pixel(Point::new(x, y)).map(rgb));
            }
        }
    }
    let mut inner = Vec::with_capacity((r.width().max(0) * r.height().max(0)) as usize);
    for y in r.top..r.bottom {
        for x in r.left..r.right {
            inner.extend(image.pixel(Point::new(x, y)).map(rgb));
        }
    }
    if ring.is_empty() {
        ring = inner.clone();
    }
    if ring.is_empty() {
        return ([255, 255, 255], [0, 0, 0], false);
    }
    let median = |channel: usize| {
        let mut v: Vec<u8> = ring.iter().map(|p| p[channel]).collect();
        v.sort_unstable();
        v[v.len() / 2]
    };
    let background = [median(0), median(1), median(2)];
    let distance = |p: &[u8; 3]| p.iter().zip(&background).map(|(a, b)| a.abs_diff(*b) as u32).sum::<u32>();
    let far = inner.iter().map(distance).max().unwrap_or(0);
    let (mut sum, mut n) = ([0u32; 3], 0u32);
    let mut strokes = 0usize;
    for p in &inner {
        let d = distance(p);
        if d * 2 >= far && far > 0 {
            sum.iter_mut().zip(p).for_each(|(s, c)| *s += *c as u32);
            n += 1;
        }
        if d * 3 >= far && far > 30 {
            strokes += 1;
        }
    }
    let mut ink = if n > 0 { sum.map(|s| (s / n) as u8) } else { [0, 0, 0] };
    let light = luminance(background) > 0.5;
    if (luminance(ink) - luminance(background)).abs() < 0.35 {
        ink = if light { [17, 17, 17] } else { [245, 245, 245] };
    }
    let bold = !inner.is_empty() && strokes as f32 / inner.len() as f32 > 0.24;
    (background, ink, bold)
}

/// The background at two opposite sides of a block, along the axis where it changes more (a
/// launcher's backdrop fades from side to side): the patch is a gradient between them, so it
/// sits in the picture instead of on it. `true`: left to right.
fn edges(image: &Image, r: Rect) -> ([u8; 3], [u8; 3], bool) {
    let median = |points: Vec<Point>| {
        let mut px: Vec<[u8; 3]> =
            points.into_iter().filter_map(|p| image.pixel(p)).map(|p| [p[2], p[1], p[0]]).collect();
        if px.is_empty() {
            return None;
        }
        let mid = px.len() / 2;
        Some([0, 1, 2].map(|c| px.select_nth_unstable_by_key(mid, |p| p[c]).1[c]))
    };
    let pad = 3;
    let column = |x: i32| (r.top..r.bottom).map(|y| Point::new(x, y)).collect::<Vec<_>>();
    let row = |y: i32| (r.left..r.right).map(|x| Point::new(x, y)).collect::<Vec<_>>();
    let (left, right) = (median(column(r.left - pad)), median(column(r.right + pad - 1)));
    let (top, bottom) = (median(row(r.top - pad)), median(row(r.bottom + pad - 1)));
    let difference = |a: Option<[u8; 3]>, b: Option<[u8; 3]>| match (a, b) {
        (Some(a), Some(b)) => a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y) as u32).sum::<u32>(),
        _ => 0,
    };
    let across = difference(left, right) >= difference(top, bottom);
    let (a, b) = if across { (left, right) } else { (top, bottom) };
    match (a, b) {
        (Some(a), Some(b)) => (a, b, across),
        _ => {
            let (background, _, _) = colours(image, r);
            (background, background, true)
        }
    }
}

fn color(c: [u8; 3]) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c[0] as f32 / 255.0, g: c[1] as f32 / 255.0, b: c[2] as f32 / 255.0, a: 1.0 }
}

// ---- text ----------------------------------------------------------------------------------

fn format(device: &Device, size: f32, weight: DWRITE_FONT_WEIGHT) -> Option<IDWriteTextFormat> {
    let shared = &device.shared;
    unsafe {
        shared
            .dwrite
            .CreateTextFormat(
                PCWSTR(shared.ui_family.as_ptr()),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("ru-ru"),
            )
            .ok()
    }
}

fn measure(device: &Device, f: &IDWriteTextFormat, value: &str, width: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
    let utf16: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        let layout = device.shared.dwrite.CreateTextLayout(&utf16, f, width, 100_000.0).ok()?;
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m).ok()?;
        Some((layout, m.width, m.height))
    }
}

/// How far block `i` may grow without covering another: right to the edge of its column (the
/// widest block starting at the same left; a lone block, the picture's edge) but not into a block
/// beside it, and down to the next block under it. Desktop pixels: (right, bottom).
fn room(rects: &[(Rect, usize)], i: usize, picture: Rect) -> (i32, i32) {
    let (r, lines) = rects[i];
    let line = (r.height() / lines.max(1) as i32).max(1);
    let others = || rects.iter().enumerate().filter(move |(j, _)| *j != i).map(|(_, (o, _))| *o);
    let column =
        others().filter(|o| (o.left - r.left).abs() <= 3 * line).map(|o| o.right).max().unwrap_or(picture.right);
    let beside =
        others().filter(|o| o.left >= r.right && o.top < r.bottom && o.bottom > r.top).map(|o| o.left - line / 2);
    let right = beside.min().map_or(column, |b| column.min(b)).max(r.right).min(picture.right);
    let below =
        others().filter(|o| o.top >= r.bottom - line / 3 && o.left < right && o.right > r.left).map(|o| o.top - 2);
    let bottom = below.min().unwrap_or(picture.bottom).max(r.bottom).min(picture.bottom);
    (right, bottom)
}

/// The translation laid out over its block: as large as the original letters and as wide as its
/// column; when longer, a little smaller, then taller into the free space under it, then smaller
/// still. It never spreads over other text: what does not fit at the smallest size is clipped.
/// `forced`: this size and no search (to match its neighbours).
fn fit(device: &Device, shown: &Shown, picture: Rect, (right, bottom): (i32, i32), forced: Option<f32>) -> Option<Fit> {
    let value = shown.block.translated.as_deref()?;
    let r = shown.block.rect;
    let (x, y) = ((r.left - picture.left) as f32, (r.top - picture.top) as f32);
    let (w, h) = (r.width() as f32, r.height() as f32);
    let line =
        if shown.block.line_height > 0.0 { shown.block.line_height } else { h / shown.block.lines.max(1) as f32 };
    let pitch = shown.block.pitch.max(line);
    let start = (line * 0.8).clamp(8.0, 120.0);
    let weight = if shown.bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
    let width = ((right - r.left) as f32).max(w);
    // Lines spaced as in the original: n lines take n pitches, a little more than the box.
    let leading = pitch - line;
    // The room under it ends where the next text starts; the last line's leading may hang over.
    let room = ((bottom - r.top) as f32).max(h);
    let tall = room + leading * 0.5;
    let attempt = |size: f32| {
        let f = format(device, size, weight)?;
        let spacing = (pitch * size / start).max(size * 1.15);
        unsafe {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let _ = f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, spacing, spacing * 0.8);
        }
        measure(device, &f, value, width)
    };
    let mut chosen = forced.and_then(|size| attempt(size).map(|(l, mw, mh)| (l, mw, mh, size)));
    'search: for (limit, floor) in [((h + leading) * 1.05, 0.84), (tall, 0.72), (tall, 0.66)] {
        let mut size = start;
        while chosen.is_none() && size >= (start * floor).max(8.0) {
            if let Some((layout, mw, mh)) = attempt(size)
                && mh <= limit
                && mw <= width + 0.5
            {
                chosen = Some((layout, mw, mh, size));
                break 'search;
            }
            size *= 0.95;
        }
    }
    let smallest = (start * 0.66).max(8.0);
    let (layout, mw, mh, size) = match chosen {
        Some(found) => found,
        None => attempt(smallest).map(|(l, mw, mh)| (l, mw, mh, smallest))?,
    };
    let pad = (line * 0.3).clamp(2.0, 12.0);
    let (pw, ph) = (picture.width() as f32, picture.height() as f32);
    // Centred on the original when it fits; a longer text starts where the original did and grows down.
    let top = y + ((h - mh) / 2.0).max(-leading / 2.0 - 1.0);
    // Up to the room, but always over the whole original.
    let max_right = (x + width).max(x + w + pad);
    let max_bottom = (y + room).max(y + h + pad);
    let patch = rect(
        (x - pad).max(0.0),
        (y.min(top) - pad).max(0.0),
        (x + w.max(mw) + pad).min(max_right).min(pw),
        ((y + h).max(top + mh) + pad).min(max_bottom).min(ph),
    );
    Some(Fit { layout, patch, at: (x, top), size, start })
}

/// One size for text of one style in one column (a list, the paragraphs of an article): the
/// smallest any of them needed, though not below 0.8 of their median, so a single long line does
/// not shrink the whole list; that line keeps its own smaller size. `None`: keep its own.
fn common_sizes(fits: &[Option<(f32, f32, i32, f32)>]) -> Vec<Option<f32>> {
    fits.iter()
        .map(|me| {
            let (size, start, left, line) = (*me)?;
            let mut peers: Vec<f32> = fits
                .iter()
                .flatten()
                .filter(|(_, s, l, _)| (s / start - 1.0).abs() < 0.12 && (l - left).abs() as f32 <= 3.0 * line)
                .map(|(size, ..)| *size)
                .collect();
            peers.sort_by(f32::total_cmp);
            let floor = peers[peers.len() / 2] * 0.8;
            let common = peers[0].max(floor);
            (size > common * 1.02).then_some(common)
        })
        .collect()
}

// ---- geometry ------------------------------------------------------------------------------

fn label(content: &Content) -> String {
    match content {
        Content::Working => "Перевожу…".into(),
        Content::Ready { blocks, from, to } => {
            if blocks.iter().all(|b| b.translated.is_none()) {
                "Переводить нечего".into()
            } else {
                format!("{} → {}", crate::bubble::language(from), crate::bubble::language(to))
            }
        }
        Content::Failed(e) => e.clone(),
    }
}

fn fonts(st: &mut State) -> Option<&Fonts> {
    let device = st.device.as_ref()?;
    if st.fonts.as_ref().is_none_or(|(s, _)| *s != st.scale) {
        let label = format(device, 13.0 * st.scale, DWRITE_FONT_WEIGHT_MEDIUM)?;
        let button = format(device, 13.0 * st.scale, DWRITE_FONT_WEIGHT_MEDIUM)?;
        unsafe {
            for f in [&label, &button] {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            }
            let _ = button.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        st.fonts = Some((st.scale, Fonts { label, button }));
    }
    st.fonts.as_ref().map(|(_, f)| f)
}

const ORIGINAL: &str = "Оригинал";
const COPY_ALL: &str = "Копировать всё";

/// The pill's width: the label, then the buttons (laid out by `pill_layout` from the right).
fn pill_width(st: &mut State) -> f32 {
    let s = st.scale;
    fonts(st);
    let (Some(device), Some((_, f))) = (st.device.as_ref(), st.fonts.as_ref()) else { return 320.0 * s };
    let width = |v: &str, f: &IDWriteTextFormat| measure(device, f, v, 100_000.0).map_or(0.0, |m| m.1);
    let label_w = width(&label(&st.content), &f.label).min(460.0 * s);
    let buttons = match st.content {
        Content::Ready { .. } => width(ORIGINAL, &f.button) + width(COPY_ALL, &f.button) + 2.0 * (24.0 + 4.0) * s,
        _ => 0.0,
    };
    let after_label = if matches!(st.content, Content::Working) { 40.0 } else { 10.0 };
    14.0 * s + label_w + after_label * s + buttons + 28.0 * s + 6.0 * s
}

/// The pill under the picture, or over it when the screen ends, or inside it as a last resort.
fn arrange(picture: Rect, work: Rect, pill: (f32, f32), s: f32) -> (Rect, D2D_RECT_F) {
    let margin = (8.0 * s) as i32;
    let (pw, ph) = (pill.0.ceil() as i32, pill.1.ceil() as i32);
    let y = if picture.bottom + margin + ph <= work.bottom {
        picture.bottom + margin
    } else if picture.top - margin - ph >= work.top {
        picture.top - margin - ph
    } else {
        picture.bottom - margin - ph
    };
    let x = ((picture.left + picture.right) / 2 - pw / 2)
        .clamp(work.left + margin, (work.right - margin - pw).max(work.left));
    let halo = (HALO * s) as i32;
    // The shadow stays on the screen (or over the picture when that is larger).
    let around = Rect::new(x - halo, y - halo, x + pw + halo, y + ph + halo);
    let around = around.intersect(&work.union(&picture)).unwrap_or(around);
    let window = picture.union(&around);
    let pill = rect(
        (x - window.left) as f32,
        (y - window.top) as f32,
        (x - window.left + pw) as f32,
        (y - window.top + ph) as f32,
    );
    (window, pill)
}

fn work_area(r: Rect) -> Rect {
    let monitor = unsafe {
        MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST)
    };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut mi);
    }
    let w = mi.rcWork;
    Rect::new(w.left, w.top, w.right, w.bottom)
}

/// Lays the window out around the picture and the pill and moves it there.
fn place(st: &mut State) {
    let Some(picture) = st.image.as_ref().map(|i| i.rect) else { return };
    let width = pill_width(st);
    let (window, pill) = arrange(picture, work_area(picture), (width, PILL_H * st.scale), st.scale);
    st.origin = (window.left, window.top);
    st.picture_at = ((picture.left - window.left) as f32, (picture.top - window.top) as f32);
    st.pill = pill;
    st.size = (window.width() as f32, window.height() as f32);
    if st.hwnd.is_invalid() {
        return;
    }
    let halo = HALO * st.scale;
    let outer = inflate(pill, halo);
    unsafe {
        let region = CreateRectRgn(
            st.picture_at.0 as i32,
            st.picture_at.1 as i32,
            st.picture_at.0 as i32 + picture.width(),
            st.picture_at.1 as i32 + picture.height(),
        );
        let around = CreateRectRgn(outer.left as i32, outer.top as i32, outer.right as i32, outer.bottom as i32);
        CombineRgn(Some(region), Some(region), Some(around), RGN_OR);
        let _ = DeleteObject(HGDIOBJ(around.0));
        let _ = SetWindowPos(
            st.hwnd,
            Some(HWND_TOPMOST),
            window.left,
            window.top,
            window.width(),
            window.height(),
            SWP_NOACTIVATE,
        );
        SetWindowRgn(st.hwnd, Some(region), true);
    }
}

/// Recomputes colours (CPU, from the picture) for a new result; layouts follow on the next paint.
fn set_content(st: &mut State, content: Content) {
    let now = st.epoch.elapsed().as_secs_f64();
    st.blocks.clear();
    st.laid_out = false;
    st.flash = None;
    st.hover = None;
    if let (Content::Ready { blocks, .. }, Some(image)) = (&content, &st.image) {
        let mut order = 0;
        for block in blocks {
            let (background, ink, bold) = colours(image, block.rect);
            let (from, to, across) = edges(image, block.rect);
            let mut appear = Spring::with(0.0, Curve::POP).precise(0.002);
            if block.translated.is_some() {
                appear.target_at(1.0, now, now + 0.04 * order as f64);
                order += 1;
            }
            st.blocks.push(Shown {
                block: block.clone(),
                background: color(background),
                sides: (color(from), color(to), across),
                brush: None,
                ink: color(ink),
                bold,
                appear,
                hover: Spring::with(0.0, Curve::HOVER_IN),
                fit: None,
            });
        }
    }
    st.original.snap(0.0);
    st.content = content;
}

fn lay_out(st: &mut State) {
    if st.laid_out {
        return;
    }
    let (Some(device), Some(image)) = (st.device.as_ref(), st.image.as_ref()) else { return };
    let rects: Vec<(Rect, usize)> = st.blocks.iter().map(|b| (b.block.rect, b.block.lines)).collect();
    for (i, b) in st.blocks.iter_mut().enumerate() {
        b.fit = fit(device, b, image.rect, room(&rects, i, image.rect), None);
    }
    let sizes: Vec<_> = st
        .blocks
        .iter()
        .map(|b| {
            let line = b.block.line_height.max(1.0);
            b.fit.as_ref().map(|f| (f.size, f.start, b.block.rect.left, line))
        })
        .collect();
    for (i, common) in common_sizes(&sizes).into_iter().enumerate() {
        if let Some(size) = common {
            let b = &st.blocks[i];
            st.blocks[i].fit = fit(device, b, image.rect, room(&rects, i, image.rect), Some(size));
        }
    }
    st.laid_out = true;
}

// ---- public --------------------------------------------------------------------------------

/// The paragraphs found on the screen, translated for the reader. Blocking (network or the local
/// translator): call from a worker. Each item of a paragraph (a sentence run, a list entry, a
/// name) is a line for the translator; the paragraph is put back from its items, untranslated
/// ones as they were.
pub fn translate(blocks: Vec<crate::ocr::Block>) -> Result<Content, String> {
    if blocks.is_empty() {
        return Err("В выделении не нашлось текста".into());
    }
    let items: Vec<(usize, String)> = blocks
        .iter()
        .enumerate()
        .flat_map(|(i, b)| b.text.split('\n').map(move |item| (i, item.to_string())))
        .collect();
    let t = crate::translate::for_reader(&items.iter().map(|(_, item)| item.clone()).collect::<Vec<_>>())?;
    let mut parts: Vec<Vec<(String, Option<String>)>> = vec![Vec::new(); blocks.len()];
    for ((i, item), translated) in items.into_iter().zip(t.texts) {
        parts[i].push((item, translated));
    }
    let blocks = blocks
        .into_iter()
        .zip(parts)
        .map(|(b, parts)| Block {
            rect: b.rect,
            lines: b.lines,
            line_height: b.line_height,
            pitch: b.pitch,
            original: b.text,
            translated: parts
                .iter()
                .any(|(_, t)| t.is_some())
                .then(|| parts.iter().map(|(item, t)| t.as_deref().unwrap_or(item)).collect::<Vec<_>>().join("\n")),
        })
        .collect();
    Ok(Content::Ready { blocks, from: t.from, to: t.to })
}

fn scale_at(r: Rect) -> f32 {
    let monitor = unsafe {
        MonitorFromPoint(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }, MONITOR_DEFAULTTONEAREST)
    };
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    dx as f32 / 96.0
}

fn new_state(hwnd: HWND, scale: f32) -> State {
    let spring = |c: Curve| Spring::with(0.0, c).precise(0.002);
    State {
        hwnd,
        device: None,
        surface: None,
        fonts: None,
        image: None,
        bitmap: None,
        content: Content::Working,
        blocks: Vec::new(),
        laid_out: false,
        scale,
        epoch: Instant::now(),
        appear: spring(Curve::POP),
        original: spring(Curve::FADE),
        buttons: [spring(Curve::HOVER_IN), spring(Curve::HOVER_IN), spring(Curve::HOVER_IN)],
        hover: None,
        tracking: false,
        flash: None,
        origin: (0, 0),
        picture_at: (0.0, 0.0),
        pill: rect(0.0, 0.0, 0.0, 0.0),
        size: (1.0, 1.0),
    }
}

/// Shows `image` where it was taken, with `content` over it.
pub fn open(instance: HINSTANCE, image: Arc<Image>, content: Content) {
    let exists = STATE.with(|s| s.borrow().is_some());
    if !exists {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrLens"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrLens"),
                w!("Kadr — перевод"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )
        };
        let Ok(hwnd) = hwnd else { return };
        STATE.with(|s| *s.borrow_mut() = Some(new_state(hwnd, 1.0)));
    }
    let hwnd = with(|st| {
        st.scale = scale_at(image.rect);
        if st.device.is_none() {
            st.device = Device::new("lens").ok();
        }
        st.image = Some(image);
        st.bitmap = None;
        set_content(st, content);
        let now = st.epoch.elapsed().as_secs_f64();
        st.appear.snap(0.0);
        st.appear.target(1.0, now);
        place(st);
        st.hwnd
    });
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        redraw(hwnd);
    }
}

/// The result for the open lens; `image` replaces the picture (Alt+click narrows it to the paragraph).
pub fn update(content: Content, image: Option<Arc<Image>>) {
    let hwnd = with(|st| {
        if let Some(image) = image {
            st.image = Some(image);
            st.bitmap = None;
        }
        set_content(st, content);
        place(st);
        st.hwnd
    });
    if let Some(hwnd) = hwnd {
        redraw(hwnd);
    }
}

pub fn close() {
    let hwnd = with(|st| {
        // The picture can be large; it is not kept once the lens is put away.
        st.image = None;
        st.bitmap = None;
        st.blocks.clear();
        st.hwnd
    });
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

// ---- drawing -------------------------------------------------------------------------------

fn paint_window(hwnd: HWND) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        let Some(device) = st.device.as_ref() else { return };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() || fonts(st).is_none() {
            return;
        }
        prepare(st);
        let st = &*st;
        let (device, surface) = (st.device.as_ref().unwrap(), st.surface.as_ref().unwrap());
        if glass::frame(device, surface, || paint(device, st)) {
            redraw(hwnd);
        }
    });
}

/// GPU resources and layouts the frame needs.
fn prepare(st: &mut State) {
    if st.bitmap.is_none()
        && let (Some(device), Some(image)) = (st.device.as_ref(), st.image.as_ref())
    {
        st.bitmap = kadr_render::annotations::bitmap(&device.gpu, image).ok();
    }
    if let Some(device) = st.device.as_ref() {
        for b in st.blocks.iter_mut().filter(|b| b.brush.is_none() && b.block.translated.is_some()) {
            b.brush = gradient(device, b.sides.0, b.sides.1);
        }
    }
    lay_out(st);
}

fn gradient(device: &Device, from: D2D1_COLOR_F, to: D2D1_COLOR_F) -> Option<ID2D1LinearGradientBrush> {
    let ctx = &device.gpu.d2d;
    let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: from }, D2D1_GRADIENT_STOP { position: 1.0, color: to }];
    unsafe {
        let target: &ID2D1RenderTarget = ctx;
        let stops = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
        ctx.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default(), None, &stops).ok()
    }
}

/// A patch with soft edges: rings fading out around a solid core, so it melts into the picture
/// around it instead of sitting on it as a box.
fn soft_patch(device: &Device, brush: &ID2D1Brush, r: D2D_RECT_F, feather: f32, alpha: f32) {
    let ctx = &device.gpu.d2d;
    let steps = 5;
    unsafe {
        for k in (1..=steps).rev() {
            let grow = feather * k as f32 / steps as f32;
            brush.SetOpacity(alpha * 0.3);
            let ring = D2D1_ROUNDED_RECT { rect: inflate(r, grow), radiusX: grow + 2.0, radiusY: grow + 2.0 };
            ctx.FillRoundedRectangle(&ring, brush);
        }
        brush.SetOpacity(alpha);
        ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 2.0, radiusY: 2.0 }, brush);
        brush.SetOpacity(1.0);
    }
}

fn badge(device: &Device, f: &Fonts, theme: &Theme, center: (f32, f32), a: f32, s: f32) {
    let r = rect(center.0 - 54.0 * s, center.1 - 13.0 * s, center.0 + 54.0 * s, center.1 + 13.0 * s);
    shadow(&device.gpu, r, 13.0 * s, s, 6.0, 2.0, 0.25 * a);
    fill(&device.gpu, fade(theme.accent, a), r, 13.0 * s);
    text(&device.gpu, &f.button, "Скопировано", r, fade(theme.on_accent, a));
}

fn flash_alpha(since: f64) -> f32 {
    (if since < 0.12 {
        since / 0.12
    } else if since < FLASH - 0.3 {
        1.0
    } else {
        (FLASH - since) / 0.3
    })
    .clamp(0.0, 1.0) as f32
}

fn paint(device: &Device, st: &State) -> bool {
    let gpu = &device.gpu;
    let ctx = &gpu.d2d;
    let Some((_, f)) = st.fonts.as_ref() else { return false };
    let theme = Theme::current();
    let now = st.epoch.elapsed().as_secs_f64();
    let s = st.scale;
    let mut moving = false;
    let mut sample = |spring: &Spring| {
        moving |= spring.active(now);
        spring.value(now)
    };
    let appear = sample(&st.appear).clamp(0.0, 1.0);
    let original = sample(&st.original);
    let (ox, oy) = st.picture_at;
    let Some(image) = &st.image else { return false };
    let (pw, ph) = (image.width() as f32, image.height() as f32);
    let picture = rect(ox, oy, ox + pw, oy + ph);
    let shift = |r: D2D_RECT_F| rect(r.left + ox, r.top + oy, r.right + ox, r.bottom + oy);
    unsafe {
        if let Some(bitmap) = &st.bitmap {
            ctx.DrawBitmap(bitmap, Some(&raw const picture), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
        }
    }
    let mut busy = false;
    match &st.content {
        Content::Working => {
            // A soft beam sweeps the picture while it is read and translated.
            busy = true;
            let t = ((now * 0.7) % 1.0) as f32;
            let band = (90.0 * s).min(ph * 0.6);
            let y = oy - band + t * (ph + 2.0 * band);
            for k in 0..20 {
                let u = k as f32 / 19.0 * 2.0 - 1.0;
                let row = rect(ox, y + u * band / 2.0, ox + pw, y + u * band / 2.0 + band / 19.0 + 0.5);
                let clipped = rect(row.left, row.top.max(oy), row.right, row.bottom.min(oy + ph));
                if clipped.bottom > clipped.top {
                    fill(gpu, fade(theme.accent, 0.20 * (-u * u * 3.0).exp() * appear), clipped, 0.0);
                }
            }
        }
        Content::Ready { .. } => {
            for b in &st.blocks {
                let hover = sample(&b.hover);
                let Some(fit) = &b.fit else {
                    if hover > 0.0 {
                        let r = &b.block.rect;
                        let r = rect(
                            (r.left - image.rect.left) as f32 + ox,
                            (r.top - image.rect.top) as f32 + oy,
                            (r.right - image.rect.left) as f32 + ox,
                            (r.bottom - image.rect.top) as f32 + oy,
                        );
                        stroke(gpu, fade(theme.accent, 0.9 * hover), inflate(r, 3.0 * s), 4.0 * s, 1.5 * s);
                    }
                    continue;
                };
                let a = sample(&b.appear).clamp(0.0, 1.0) * (1.0 - original);
                if a <= 0.0 {
                    continue;
                }
                let patch = shift(fit.patch);
                let feather = (b.block.line_height * 0.45).clamp(2.0, 14.0);
                match &b.brush {
                    Some(brush) => unsafe {
                        let (across, (l, t, r, btm)) = (b.sides.2, (patch.left, patch.top, patch.right, patch.bottom));
                        brush.SetStartPoint(Vector2 { X: l, Y: t });
                        brush.SetEndPoint(if across { Vector2 { X: r, Y: t } } else { Vector2 { X: l, Y: btm } });
                        soft_patch(device, brush, patch, feather, a);
                    },
                    None => {
                        unsafe { gpu.brush.SetColor(&b.background) };
                        soft_patch(device, &gpu.brush.cast().unwrap(), patch, feather, a);
                    }
                }
            }
            // The words after all the patches: a neighbour's soft edge never covers them.
            for b in &st.blocks {
                let Some(fit) = &b.fit else { continue };
                let a = sample(&b.appear).clamp(0.0, 1.0) * (1.0 - original);
                if a <= 0.0 {
                    continue;
                }
                let patch = shift(fit.patch);
                unsafe {
                    ctx.PushAxisAlignedClip(&patch, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    gpu.brush.SetColor(&fade(b.ink, a));
                    ctx.DrawTextLayout(
                        Vector2 { X: fit.at.0 + ox, Y: fit.at.1 + oy },
                        &fit.layout,
                        &gpu.brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                    ctx.PopAxisAlignedClip();
                }
            }
            for b in &st.blocks {
                let Some(fit) = &b.fit else { continue };
                let hover = sample(&b.hover);
                if hover > 0.0 {
                    let ring = inflate(shift(fit.patch), 3.0 * s);
                    fill(gpu, fade(theme.accent, 0.08 * hover), ring, 6.0 * s);
                    stroke(gpu, fade(theme.accent, 0.85 * hover), ring, 6.0 * s, 1.5 * s);
                }
            }
        }
        Content::Failed(_) => {}
    }
    // A hairline marks the frozen picture as Kadr's, not the live screen.
    stroke(gpu, fade(theme.accent, 0.4 * appear), inflate(picture, -0.5 * s), 0.0, 1.0 * s);
    if let Some((which, at)) = st.flash {
        let since = now - at;
        if since < FLASH {
            busy = true;
            let a = flash_alpha(since);
            let center = match which.and_then(|i| st.blocks.get(i)) {
                Some(b) => {
                    let r = match &b.fit {
                        Some(fit) => shift(fit.patch),
                        None => {
                            let r = &b.block.rect;
                            rect(
                                (r.left - image.rect.left) as f32 + ox,
                                (r.top - image.rect.top) as f32 + oy,
                                (r.right - image.rect.left) as f32 + ox,
                                (r.bottom - image.rect.top) as f32 + oy,
                            )
                        }
                    };
                    let above = r.top - 18.0 * s;
                    let y = if above - 13.0 * s >= 0.0 { above } else { r.bottom + 18.0 * s };
                    ((r.left + r.right) / 2.0, y)
                }
                None => ((picture.left + picture.right) / 2.0, (picture.top + picture.bottom) / 2.0),
            };
            badge(device, f, &theme, center, a, s);
        }
    }
    // The pill.
    let pill = st.pill;
    let pop = 0.94 + 0.06 * appear;
    let lift = (1.0 - appear) * 6.0 * s;
    unsafe {
        ctx.SetTransform(&glass::scaled_about(pill, pop, 0.0, lift));
    }
    shadow(gpu, pill, PILL_H * s / 2.0, s, 10.0, 3.0, 0.32 * appear);
    fill(gpu, fade(theme.surface, appear), pill, PILL_H * s / 2.0);
    stroke(gpu, fade(theme.border, appear), inflate(pill, -0.5), PILL_H * s / 2.0, 1.0);
    let at = |r: D2D_RECT_F| rect(r.left + pill.left, r.top + pill.top, r.right + pill.left, r.bottom + pill.top);
    let (label_rect, parts) = pill_layout(st);
    let tone = if matches!(st.content, Content::Failed(_)) { glass::error_ink(&theme) } else { theme.text_muted };
    let label_rect = at(label_rect);
    let value: Vec<u16> = label(&st.content).encode_utf16().collect();
    unsafe {
        gpu.brush.SetColor(&fade(tone, appear));
        ctx.DrawText(
            &value,
            &f.label,
            &label_rect,
            &gpu.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    if busy && matches!(st.content, Content::Working) {
        for i in 0..3 {
            let t = now as f32 * 5.0 - i as f32 * 0.7;
            let pulse = 0.35 + 0.65 * (0.5 + 0.5 * t.sin());
            let (x, y) =
                (label_rect.right + 12.0 * s + i as f32 * 10.0 * s, (label_rect.top + label_rect.bottom) / 2.0);
            fill(
                gpu,
                fade(theme.text_muted, pulse * appear),
                rect(x - 3.0 * s, y - 3.0 * s, x + 3.0 * s, y + 3.0 * s),
                3.0 * s,
            );
        }
    }
    for (hit, r) in parts {
        let r = at(r);
        let index = match hit {
            Hit::Original => 0,
            Hit::CopyAll => 1,
            _ => 2,
        };
        let hover = sample(&st.buttons[index]);
        match hit {
            Hit::Close => {
                fill(gpu, fade(theme.tint, appear * hover), r, r.bottom - r.top);
                let (cx, cy, k) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, 4.0 * s);
                unsafe {
                    gpu.brush.SetColor(&fade(mix(theme.text_muted, theme.text, hover), appear));
                    for (p, q) in [((-k, -k), (k, k)), ((-k, k), (k, -k))] {
                        ctx.DrawLine(
                            Vector2 { X: cx + p.0, Y: cy + p.1 },
                            Vector2 { X: cx + q.0, Y: cy + q.1 },
                            &gpu.brush,
                            1.4 * s,
                            None,
                        );
                    }
                }
            }
            _ => {
                let on = hit == Hit::Original && st.original.goal() > 0.5;
                let base = if on { theme.selected } else { theme.tint };
                fill(gpu, fade(mix(base, theme.tint_strong, hover * 0.6), appear), r, 8.0 * s);
                let ink = if on { theme.on_selected } else { theme.text };
                text(gpu, &f.button, if hit == Hit::Original { ORIGINAL } else { COPY_ALL }, r, fade(ink, appear));
            }
        }
    }
    unsafe {
        ctx.SetTransform(&windows_numerics::Matrix3x2::identity());
    }
    moving || busy
}

/// The pill's label and buttons relative to its top-left; buttons from the right edge.
fn pill_layout(st: &State) -> (D2D_RECT_F, Vec<(Hit, D2D_RECT_F)>) {
    let s = st.scale;
    let width = st.pill.right - st.pill.left;
    let (bh, gap, pad) = (28.0 * s, 4.0 * s, 6.0 * s);
    let top = (PILL_H * s - bh) / 2.0;
    let mut parts = Vec::new();
    let mut x = width - pad - bh;
    parts.push((Hit::Close, rect(x, top, x + bh, top + bh)));
    if matches!(st.content, Content::Ready { .. })
        && let Some((_, f)) = st.fonts.as_ref()
        && let Some(device) = st.device.as_ref()
    {
        for (hit, value) in [(Hit::CopyAll, COPY_ALL), (Hit::Original, ORIGINAL)] {
            let w = measure(device, &f.button, value, 100_000.0).map_or(80.0 * s, |m| m.1) + 24.0 * s;
            x -= gap + w;
            parts.push((hit, rect(x, top, x + w, top + bh)));
        }
    }
    let end = if matches!(st.content, Content::Ready { .. }) { x - 10.0 * s } else { x - 40.0 * s };
    let label_rect = rect(14.0 * s, 0.0, end.max(14.0 * s), PILL_H * s);
    (label_rect, parts)
}

// ---- input ---------------------------------------------------------------------------------

fn hit(st: &State, x: f32, y: f32) -> Option<Hit> {
    let pill = st.pill;
    if inside(pill, x, y) {
        let (_, parts) = pill_layout(st);
        return parts
            .into_iter()
            .find(|(_, r)| {
                inside(rect(r.left + pill.left, r.top + pill.top, r.right + pill.left, r.bottom + pill.top), x, y)
            })
            .map(|(h, _)| h);
    }
    let image = st.image.as_ref()?;
    let (px, py) = (x - st.picture_at.0, y - st.picture_at.1);
    if !matches!(st.content, Content::Ready { .. }) {
        return None;
    }
    // Later blocks are drawn on top: look at them first.
    st.blocks.iter().enumerate().rev().find_map(|(i, b)| {
        let r = match &b.fit {
            Some(fit) if st.original.goal() < 0.5 => fit.patch,
            _ => {
                let r = &b.block.rect;
                inflate(
                    rect(
                        (r.left - image.rect.left) as f32,
                        (r.top - image.rect.top) as f32,
                        (r.right - image.rect.left) as f32,
                        (r.bottom - image.rect.top) as f32,
                    ),
                    3.0,
                )
            }
        };
        inside(r, px, py).then_some(Hit::Block(i))
    })
}

fn set_hover(st: &mut State, h: Option<Hit>) {
    if st.hover == h {
        return;
    }
    st.hover = h;
    let now = st.epoch.elapsed().as_secs_f64();
    let aim = |spring: &mut Spring, on: bool| {
        spring.target_with(f32::from(on), if on { Curve::HOVER_IN } else { Curve::HOVER_OUT }, now);
    };
    for (i, b) in st.blocks.iter_mut().enumerate() {
        aim(&mut b.hover, h == Some(Hit::Block(i)));
    }
    for (i, which) in [Hit::Original, Hit::CopyAll, Hit::Close].into_iter().enumerate() {
        aim(&mut st.buttons[i], h == Some(which));
    }
}

/// All of it in reading order: translations where there are, the original elsewhere.
fn whole_text(st: &State) -> String {
    let show_original = st.original.goal() > 0.5;
    st.blocks
        .iter()
        .map(|b| match (&b.block.translated, show_original) {
            (Some(t), false) => t.as_str(),
            _ => b.block.original.as_str(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn copy(st: &mut State, which: Option<usize>) {
    let value = match which.and_then(|i| st.blocks.get(i)) {
        Some(b) if st.original.goal() < 0.5 => b.block.translated.clone().unwrap_or_else(|| b.block.original.clone()),
        Some(b) => b.block.original.clone(),
        None => whole_text(st),
    };
    if value.trim().is_empty() {
        return;
    }
    match kadr_export::clipboard::put_text(st.hwnd, &value) {
        Ok(()) => st.flash = Some((which, st.epoch.elapsed().as_secs_f64())),
        Err(e) => kadr_win::log_warn!("перевод в буфер: {e}"),
    }
}

fn toggle_original(st: &mut State) {
    if !matches!(st.content, Content::Ready { .. }) {
        return;
    }
    let now = st.epoch.elapsed().as_secs_f64();
    let goal = if st.original.goal() > 0.5 { 0.0 } else { 1.0 };
    st.original.target(goal, now);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let point = || ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
            }
            paint_window(hwnd);
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        // Clicking anywhere else puts it away, like the bubble.
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            close();
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let key = wp.0 as u16;
            if key == VK_ESCAPE.0 {
                close();
            } else if key == VK_SPACE.0 {
                with(toggle_original);
            } else if ctrl && key == u16::from(b'C') {
                with(|st| copy(st, None));
            }
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            let track = with(|st| {
                let h = hit(st, x, y);
                set_hover(st, h);
                !std::mem::replace(&mut st.tracking, true)
            });
            if track == Some(true) {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut t);
                }
            }
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|st| {
                st.tracking = false;
                set_hover(st, None);
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let hand = with(|st| st.hover.is_some()).unwrap_or(false);
            unsafe {
                SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
            }
            return LRESULT(1);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            let close_it = with(|st| match hit(st, x, y) {
                Some(Hit::Close) => true,
                Some(Hit::Original) => {
                    toggle_original(st);
                    false
                }
                Some(Hit::CopyAll) => {
                    copy(st, None);
                    false
                }
                Some(Hit::Block(i)) => {
                    copy(st, Some(i));
                    false
                }
                None => false,
            });
            if close_it == Some(true) {
                close();
            }
            redraw(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

// ---- design snapshot -----------------------------------------------------------------------

/// Text drawn into the preview: value, x, y, wrap width, size, weight, colour, its translation.
type Sample = (&'static str, f32, f32, f32, f32, DWRITE_FONT_WEIGHT, u32, &'static str);

/// A news page in a game launcher: paragraphs and a dense changelog, where the Russian is longer.
const NEWS: [Sample; 8] = [
    ("Hello, Forts fans!", 20.0, 16.0, 760.0, 24.0, DWRITE_FONT_WEIGHT_BOLD, 0xffffff, "Добрый день, фанаты Forts!"),
    (
        "In this update we are making balance changes, mostly to the Firebird weapons. We made the Water Cannon available in multiplayer and skirmish. The Firebird commander can build two, the rest just one, and it fires burning shots.",
        20.0,
        60.0,
        760.0,
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
        0xc6d4df,
        "В этом обновлении мы вносим изменения в баланс, в основном в оружие Жар-птицы. Водомёт теперь доступен в многопользовательском режиме и в перестрелке. Командир Жар-птицы может построить два, остальные — только один, и он стреляет пылающими снарядами.",
    ),
    (
        "In ranked matches you can now reveal your name either while loading or at the end of the match. Both players must agree for this to work.",
        20.0,
        140.0,
        760.0,
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
        0xc6d4df,
        "В рейтинговых матчах теперь можно раскрыть своё имя во время загрузки или в конце матча. Чтобы это сработало, согласиться должны оба игрока.",
    ),
    (
        "Changelog 1.39.0 r22735",
        20.0,
        206.0,
        760.0,
        20.0,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        0xffffff,
        "Список изменений 1.39.0 r22735",
    ),
    (
        "Added: AntiAirFireTimeMin, which overrides AntiAirFireLeadTimeMin so domed AA weapons fire with fewer restrictions.",
        20.0,
        246.0,
        780.0,
        13.0,
        DWRITE_FONT_WEIGHT_NORMAL,
        0xc6d4df,
        "Добавлено: AntiAirFireTimeMin, которое переопределяет AntiAirFireLeadTimeMin, чтобы купольные зенитные орудия стреляли с меньшими ограничениями.",
    ),
    (
        "Added: AntiAirInclude and AntiAirExclude ammo properties.",
        20.0,
        268.0,
        780.0,
        13.0,
        DWRITE_FONT_WEIGHT_NORMAL,
        0xc6d4df,
        "Добавлено: свойства боеприпасов AntiAirInclude и AntiAirExclude.",
    ),
    (
        "Added: AntiAirIncludeConditions ammo property to target specific AA ammo based on projectile data.",
        20.0,
        290.0,
        780.0,
        13.0,
        DWRITE_FONT_WEIGHT_NORMAL,
        0xc6d4df,
        "Добавлено: свойство боеприпасов AntiAirIncludeConditions, позволяющее целиться в конкретные боеприпасы ПВО по данным снаряда.",
    ),
    (
        "Fixed many desyncs, mission problems and security issues.",
        20.0,
        312.0,
        780.0,
        13.0,
        DWRITE_FONT_WEIGHT_NORMAL,
        0xc6d4df,
        "Исправлено множество рассинхронизаций, проблем с миссиями и уязвимостей.",
    ),
];

/// A synthetic dark game panel and a light card (or, `long`, a news page), translated, with the
/// pointer over one block.
pub fn preview(scale: f32, long: bool) -> windows::core::Result<Image> {
    let device = Device::new("lens-preview")?;
    let s = scale;
    let (w, h) = if long { ((820.0 * s) as u32, (345.0 * s) as u32) } else { ((760.0 * s) as u32, (300.0 * s) as u32) };
    let card: [Sample; 6] = [
        ("Team settings", 24.0, 22.0, 400.0, 24.0, DWRITE_FONT_WEIGHT_BOLD, 0xffffff, "Настройки команды"),
        (
            "Choose a map and invite your friends. The match starts when everyone is ready.",
            24.0,
            66.0,
            380.0,
            15.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            0xc9d1e0,
            "Выберите карту и пригласите друзей. Матч начнётся, когда все будут готовы.",
        ),
        ("anerurkiola", 24.0, 150.0, 300.0, 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, 0xffffff, ""),
        ("Ready", 24.0, 180.0, 300.0, 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, 0x7ee787, "Готов"),
        ("Наблюдатель", 24.0, 240.0, 300.0, 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, 0xffffff, ""),
        ("Save changes", 476.0, 132.0, 240.0, 16.0, DWRITE_FONT_WEIGHT_MEDIUM, 0x1b1f24, "Сохранить изменения"),
    ];
    let samples: &[Sample] = if long { &NEWS } else { &card };
    let mut blocks = Vec::new();
    let picture = glass::render_offscreen(&device, w, h, || {
        let gpu = &device.gpu;
        if long {
            fill(gpu, glass::rgb(0x1b2838, 1.0), rect(0.0, 0.0, w as f32, h as f32), 0.0);
        } else {
            fill(gpu, glass::rgb(0x1e2430, 1.0), rect(0.0, 0.0, w as f32, h as f32), 0.0);
            fill(gpu, glass::rgb(0x2a3242, 1.0), rect(12.0 * s, 136.0 * s, 330.0 * s, 222.0 * s), 8.0 * s);
            fill(gpu, glass::rgb(0xf3f4f6, 1.0), rect(452.0 * s, 104.0 * s, 736.0 * s, 196.0 * s), 12.0 * s);
        }
        for (value, x, y, width, size, weight, ink, translated) in samples.iter().copied() {
            let Some(f) = format(&device, size * s, weight) else { continue };
            let Some((layout, mw, mh)) = measure(&device, &f, value, width * s) else { continue };
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe {
                let _ = layout.GetMetrics(&mut m);
                gpu.brush.SetColor(&glass::rgb(ink, 1.0));
                gpu.d2d.DrawTextLayout(
                    Vector2 { X: x * s, Y: y * s },
                    &layout,
                    &gpu.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            let (x, y) = ((x * s) as i32, (y * s) as i32);
            blocks.push(Block {
                rect: Rect::new(x, y, x + mw.ceil() as i32, y + mh.ceil() as i32),
                lines: m.lineCount.max(1) as usize,
                line_height: mh / m.lineCount.max(1) as f32 * 0.82,
                pitch: mh / m.lineCount.max(1) as f32,
                original: value.into(),
                translated: (!translated.is_empty()).then(|| translated.into()),
            });
        }
    })?;
    let picture = Arc::new(Image::new(Rect::new(0, 0, w as i32, h as i32), picture.bgra));
    let hover = if long { NEWS[2].0 } else { "Save changes" };
    render(device, picture, Content::Ready { blocks, from: "en".into(), to: "ru".into() }, scale, Some(hover))
}

/// The lens as it would open over `picture`, fully appeared, the pointer over the block whose
/// original is `hover`; the pill below it.
fn render(
    device: Device,
    picture: Arc<Image>,
    content: Content,
    scale: f32,
    hover: Option<&str>,
) -> windows::core::Result<Image> {
    let s = scale;
    let mut st = new_state(HWND::default(), scale);
    st.device = Some(device);
    st.image = Some(picture.clone());
    set_content(&mut st, content);
    let r = picture.rect;
    let screen = Rect::new(r.left, r.top, r.right, r.bottom + (80.0 * s) as i32);
    let width = pill_width(&mut st);
    let (window, pill) = arrange(picture.rect, screen, (width, PILL_H * s), s);
    st.picture_at = ((picture.rect.left - window.left) as f32, (picture.rect.top - window.top) as f32);
    st.pill = pill;
    st.size = (window.width() as f32, window.height() as f32);
    st.appear.snap(1.0);
    for b in &mut st.blocks {
        b.appear.snap(1.0);
    }
    prepare(&mut st);
    if let Some(i) = st.blocks.iter().position(|b| Some(b.block.original.as_str()) == hover) {
        st.hover = Some(Hit::Block(i));
        st.blocks[i].hover.snap(1.0);
    }
    let device = st.device.as_ref().unwrap();
    glass::render_offscreen(device, window.width() as u32, window.height() as u32, || {
        paint(device, &st);
    })
}

// ---- development check -----------------------------------------------------------------------

/// The text of a game launcher's news page, as wide paragraphs and a change list.
const PAGE: [(&str, f32, DWRITE_FONT_WEIGHT); 11] = [
    ("G'day Forts fans!", 30.0, DWRITE_FONT_WEIGHT_BOLD),
    (
        "In this update we're dropping a volatile canister's worth of balance changes, mostly to the Firebird weapons. We've made the Water Cannon available in multiplayer and skirmish modes. The Firebird commander can build two while others only one, and hers will fire at flaming projectiles.",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
    (
        "Co-op on Elimination has been refreshed with voting and weapons only mode. You can either reject a joining player altogether, or give them the ability to only fire and repair. The lobby can be set to 'Weapons Only' so there's no option of full control.",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
    (
        "In ranked matches you can now opt-in to reveal your name either during loading, or at the end of the match. You can also chose to chat with your opponent. Both players must agree for this to work.",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
    (
        "We've fixed a load of crashes, desyncs, mission problems, security issues, and other misc bugs. As always let us know if you find more.",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
    ("Cheers,\nEWG", 15.0, DWRITE_FONT_WEIGHT_NORMAL),
    ("Change List 1.39.0 r22735", 26.0, DWRITE_FONT_WEIGHT_NORMAL),
    ("Add: (Firebird passive) Water Cannon shoots at flaming projectiles", 15.0, DWRITE_FONT_WEIGHT_NORMAL),
    (
        "Add: AntiAirFireTimeMin to override AntiAirFireLeadTimeMin to make dome style AA weapons fire with less restriction",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
    (
        "Add: AntiAirInclude and AntiAirExclude ammo properties which act the same as those in ai.lua to allow ammo specific AA targeting",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
    (
        "Add: AntiAirIncludeConditions ammo property which allows ammo specific AA targeting based on projectile data",
        15.0,
        DWRITE_FONT_WEIGHT_NORMAL,
    ),
];

/// The news page drawn at `scale` on a launcher's dim backdrop that warms from left to right.
fn page(device: &Device, scale: f32) -> windows::core::Result<Image> {
    let s = scale;
    let (w, h) = ((840.0 * s) as u32, (700.0 * s) as u32);
    let picture = glass::render_offscreen(device, w, h, || {
        let gpu = &device.gpu;
        if let Some(brush) = gradient(device, glass::rgb(0x343a4e, 1.0), glass::rgb(0x4a3d58, 1.0)) {
            unsafe {
                brush.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 });
                brush.SetEndPoint(Vector2 { X: w as f32, Y: 0.0 });
                gpu.d2d.FillRectangle(&rect(0.0, 0.0, w as f32, h as f32), &brush);
            }
        }
        let mut y = 20.0 * s;
        for (value, size, weight) in PAGE {
            let Some(f) = format(device, size * s, weight) else { continue };
            unsafe {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            }
            let Some((layout, _, mh)) = measure(device, &f, value, 800.0 * s) else { continue };
            unsafe {
                gpu.brush.SetColor(&glass::rgb(0xe6e9ef, 1.0));
                gpu.d2d.DrawTextLayout(Vector2 { X: 20.0 * s, Y: y }, &layout, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            let list = value.starts_with("Add:");
            y += mh + if list { 6.0 } else { 20.0 } * s;
        }
    })?;
    Ok(Image::new(Rect::new(0, 0, w as i32, h as i32), picture.bgra))
}

/// Any picture Windows can open, as opaque BGRA.
fn load(path: &std::path::Path) -> windows::core::Result<Image> {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat};
    use windows::Security::Cryptography::CryptographicBuffer;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    let bytes = std::fs::read(path)
        .map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(&bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let bitmap = decoder.GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Ignore)?.join()?;
    let (w, h) = (bitmap.PixelWidth()?, bitmap.PixelHeight()?);
    let buffer = windows::Storage::Streams::Buffer::Create((w * h * 4) as u32)?;
    bitmap.CopyToBuffer(&buffer)?;
    let mut data = windows::core::Array::<u8>::new();
    CryptographicBuffer::CopyToByteArray(&buffer, &mut data)?;
    let mut bgra = data.to_vec();
    for px in bgra.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    Ok(Image::new(Rect::new(0, 0, w, h), bgra))
}

/// `kadr.exe --lens-check out.png [in.png]`: the real recognition and translation of a picture
/// (or of the news page above), drawn as the lens would show it. Only the recognised text goes to
/// the translator, as in Kadr itself. The paragraphs found are printed.
pub fn check(output: &std::path::Path, input: Option<&std::path::Path>, scale: f32) -> Result<(), String> {
    let device = Device::new("lens-check").map_err(|e| e.message())?;
    let picture = match input {
        Some(path) => load(path),
        None => page(&device, scale),
    }
    .map_err(|e| format!("картинка: {}", e.message()))?;
    let lines = crate::ocr::recognize(&picture).map_err(|e| format!("распознавание: {}", e.message()))?;
    let blocks = crate::ocr::blocks(&lines);
    for b in &blocks {
        println!("[{} строк, {:?}] {}", b.lines, b.rect, b.text.replace('\n', " ⏎ "));
    }
    let content = translate(blocks)?;
    if let Content::Ready { blocks, from, to } = &content {
        println!("--- {from} → {to}");
        for b in blocks {
            println!("{}", b.translated.as_deref().unwrap_or("(как есть)").replace('\n', " ⏎ "));
        }
    }
    let image = render(device, Arc::new(picture), content, scale, None).map_err(|e| e.message())?;
    std::fs::write(output, kadr_doc::pixels::encode_png(&image).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn picture(w: i32, h: i32, background: [u8; 3]) -> Image {
        let px = [background[2], background[1], background[0], 255];
        Image::new(Rect::new(100, 50, 100 + w, 50 + h), px.repeat((w * h) as usize))
    }
    fn paint_rect(image: &mut Image, r: Rect, c: [u8; 3]) {
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let i = (((y - image.rect.top) * image.rect.width() + (x - image.rect.left)) * 4) as usize;
                image.bgra[i..i + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
    }
    #[test]
    fn colours_come_from_the_ring_around_the_text_and_its_strokes() {
        let mut image = picture(200, 80, [30, 36, 48]);
        let block = Rect::new(120, 60, 220, 80);
        // Thin light-green strokes inside the block.
        for x in (122..218).step_by(6) {
            paint_rect(&mut image, Rect::new(x, 63, x + 1, 77), [126, 231, 135]);
        }
        let (background, ink, bold) = colours(&image, block);
        assert_eq!(background, [30, 36, 48]);
        assert_eq!(ink, [126, 231, 135]);
        assert!(!bold);
        // Ink as faint as the background turns into readable white on dark.
        let mut faint = picture(200, 80, [30, 36, 48]);
        paint_rect(&mut faint, Rect::new(130, 65, 140, 75), [40, 44, 56]);
        assert_eq!(colours(&faint, block).1, [245, 245, 245]);
    }
    #[test]
    fn the_pill_goes_below_then_above_then_inside() {
        let work = Rect::new(0, 0, 1920, 1040);
        let (window, pill) = arrange(Rect::new(100, 100, 600, 300), work, (300.0, 40.0), 1.0);
        assert!(pill.top >= 200.0 + 8.0, "below the picture: {pill:?}");
        assert_eq!((window.left, window.top), (100, 100));
        let (_, pill) = arrange(Rect::new(100, 900, 600, 1030), work, (300.0, 40.0), 1.0);
        assert!(pill.bottom <= 14.0 + 40.0 + 0.5 && pill.top >= 0.0, "above: {pill:?}");
        let (window, pill) = arrange(Rect::new(0, 0, 1920, 1040), work, (300.0, 40.0), 1.0);
        assert_eq!(window, Rect::new(0, 0, 1920, 1040));
        assert!(pill.bottom <= 1040.0 - 8.0 + 0.5, "inside: {pill:?}");
    }
    #[test]
    fn copy_all_keeps_reading_order_and_the_untranslated_originals() {
        let mut st = new_state(HWND::default(), 1.0);
        let block = |o: &str, t: Option<&str>| Block {
            rect: Rect::new(0, 0, 10, 10),
            lines: 1,
            line_height: 10.0,
            pitch: 10.0,
            original: o.into(),
            translated: t.map(Into::into),
        };
        st.image = Some(Arc::new(picture(20, 20, [255, 255, 255])));
        set_content(
            &mut st,
            Content::Ready {
                blocks: vec![block("Settings", Some("Настройки")), block("StarKoff", None)],
                from: "en".into(),
                to: "ru".into(),
            },
        );
        assert_eq!(whole_text(&st), "Настройки\nStarKoff");
        assert_eq!(label(&st.content), "Английский → Русский");
        toggle_original(&mut st);
        assert_eq!(whole_text(&st), "Settings\nStarKoff");
    }
    /// The Steam news case: translations spread to the screen edge and over the next lines.
    #[test]
    fn room_stops_at_the_column_the_block_beside_and_the_line_below() {
        let picture = Rect::new(0, 0, 1600, 1000);
        let rects = [
            (Rect::new(20, 100, 600, 116), 1), // a short line of a list
            (Rect::new(20, 122, 900, 138), 1), // the widest line: the column's edge
            (Rect::new(20, 144, 300, 160), 1),
            (Rect::new(1000, 98, 1200, 118), 1), // another panel to the right
        ];
        assert_eq!(room(&rects, 0, picture), (900, 120), "to the column's edge, down to the next line");
        assert_eq!(room(&rects, 2, picture), (900, 1000), "the last one may grow down");
        assert_eq!(room(&rects, 3, picture), (1600, 1000));
        let beside = [(Rect::new(20, 100, 600, 116), 1), (Rect::new(700, 100, 900, 116), 1)];
        assert_eq!(room(&beside, 0, picture).0, 692, "stops before the block beside it");
    }
    #[test]
    fn one_style_in_one_column_gets_one_size_but_one_long_line_does_not_shrink_all() {
        // (size it fitted at, natural size, left, line height)
        let list = [
            Some((13.0, 13.0, 20, 17.0)),
            Some((11.5, 13.0, 20, 17.0)),
            Some((12.0, 13.0, 20, 17.0)),
            Some((9.0, 13.0, 20, 17.0)),  // one very long line
            Some((20.0, 20.0, 20, 25.0)), // a heading: another style
            None,
        ];
        let common = common_sizes(&list);
        // Median 12 → floor 9.6: the list goes to 9.6, the long line keeps its 9.
        assert_eq!(common[0], Some(12.0 * 0.8));
        assert_eq!(common[1], Some(12.0 * 0.8));
        assert_eq!(common[3], None);
        assert_eq!((common[4], common[5]), (None, None));
    }
}
