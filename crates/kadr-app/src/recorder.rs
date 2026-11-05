//! Video of one screen area with the computer's sound, into an MP4. The picture comes from DXGI
//! desktop duplication (the cursor drawn in with GDI, since duplication leaves it out), the sound
//! from WASAPI loopback (whatever the speakers play; the microphone mixed in on request), and Media
//! Foundation's own H.264 and AAC encoders write the file — nothing beyond Windows itself. Everything
//! runs on one worker thread with its own capture device, so the app stays responsive and the sink
//! writer is never shared.
use crate::gif_writer::GifWriter;
use kadr_doc::{Image, Rect};
use kadr_win::{log_info, log_warn};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HMODULE, POINT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, HICON, ICONINFO,
};
use windows::core::{Error, HSTRING, Interface, Result};

const FPS: u32 = 30;
/// One frame in Media Foundation's 100 ns units.
const FRAME: i64 = 10_000_000 / FPS as i64;
/// GIF runs at 15 fps: smooth enough for a screen, half the frames to encode and send.
const GIF_FRAME: i64 = 10_000_000 / 15;

pub enum Event {
    /// Recording runs: how long so far.
    Tick(Duration),
    Done(std::result::Result<Finished, String>),
}

pub struct Finished {
    pub path: PathBuf,
    /// The last frame, for the corner thumbnail.
    pub preview: Image,
    pub seconds: f64,
}

pub struct Recording {
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    cursor: Arc<AtomicBool>,
}

impl Recording {
    /// Finish and keep the file.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    /// Finish and delete the file.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.stop.store(true, Ordering::Relaxed);
    }
    /// Whether the mouse pointer is drawn into the frames from now on.
    pub fn set_cursor(&self, on: bool) {
        self.cursor.store(on, Ordering::Relaxed);
    }
}

/// Starts recording `region` (desktop pixels) into `path`, with the microphone mixed in when
/// `microphone` and the pointer drawn in when `cursor`; `notify` is called from the worker.
pub fn start_with_audio(
    region: Rect,
    path: PathBuf,
    system_audio: bool,
    microphone: bool,
    cursor: bool,
    notify: impl Fn(Event) + Send + 'static,
) -> Recording {
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    let cursor = Arc::new(AtomicBool::new(cursor));
    let (s, c, p) = (stop.clone(), cancel.clone(), cursor.clone());
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let result = record(region, &path, (system_audio, microphone), &s, &c, &p, &notify);
        if c.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&path);
        }
        notify(Event::Done(result.map_err(|e| e.message())));
    });
    Recording { stop, cancel, cursor }
}

/// The area as the encoder needs it: on one monitor, even width and height.
pub fn fit(region: Rect, monitor: Rect) -> Option<Rect> {
    let r = region.intersect(&monitor)?;
    let (w, h) = (r.width() & !1, r.height() & !1);
    (w >= 16 && h >= 16).then(|| Rect::new(r.left, r.top, r.left + w, r.top + h))
}

fn record(
    region: Rect,
    path: &std::path::Path,
    audio: (bool, bool),
    stop: &AtomicBool,
    cancel: &AtomicBool,
    cursor: &AtomicBool,
    notify: &dyn Fn(Event),
) -> Result<Finished> {
    let (adapters, mut outputs) = kadr_capture::enumerate()?;
    let center = kadr_doc::Point::new((region.left + region.right) / 2, (region.top + region.bottom) / 2);
    let out = outputs
        .iter_mut()
        .find(|o| o.bounds().contains(center))
        .ok_or_else(|| Error::new(windows::Win32::Foundation::E_FAIL, "Область вне мониторов"))?;
    let region = fit(region, out.bounds())
        .ok_or_else(|| Error::new(windows::Win32::Foundation::E_FAIL, "Слишком маленькая область для видео"))?;
    let (w, h) = (region.width() as u32, region.height() as u32);
    let (device, context) = device(&adapters[out.gpu].adapter.cast()?)?;
    let staging = unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut t = None;
        device.CreateTexture2D(&desc, None, Some(&mut t))?;
        t.ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?
    };
    let mut canvas = Canvas::new(w, h)?;
    // A `.gif` path: a silent animation for chats, at a calmer pace; anything else is MP4 with sound.
    let gif = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    let frame = if gif { GIF_FRAME } else { FRAME };
    let mut sound = if gif { None } else { Sound::start(audio.0, audio.1) };
    let sink = if gif {
        Sink::Gif(GifWriter::new(path, w, h).map_err(|e| Error::new(windows::Win32::Foundation::E_FAIL, e))?)
    } else {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL)? };
        match Writer::new(path, w, h, sound.as_ref().map(|s| s.rate)) {
            Ok(w) => Sink::Mp4(w),
            Err(e) => {
                unsafe {
                    let _ = MFShutdown();
                }
                return Err(e);
            }
        }
    };
    log_info!(
        "запись{}: {w}×{h}, {} к/с, звук {}",
        if gif { " GIF" } else { "" },
        10_000_000 / frame,
        sound.as_ref().map_or("нет".to_string(), Sound::describe)
    );

    let origin = out.bounds();
    let source = D3D11_BOX {
        left: (region.left - origin.left) as u32,
        top: (region.top - origin.top) as u32,
        front: 0,
        right: (region.right - origin.left) as u32,
        bottom: (region.bottom - origin.top) as u32,
        back: 1,
    };
    let started = Instant::now();
    let mut frames: i64 = 0;
    let mut last_tick: Option<Duration> = None;
    let mut result = Ok(());
    // At least one frame, however soon the stop comes: a file without samples cannot be finished.
    loop {
        let elapsed = started.elapsed();
        // The picture: a fresh frame if the screen changed, else the previous one again.
        match out.capture(&device, &context) {
            Ok(_) => {
                if let Some(frame) = &out.frame {
                    unsafe { context.CopySubresourceRegion(&staging, 0, 0, 0, 0, frame, 0, Some(&source)) };
                    if let Err(e) = canvas.read(&context, &staging) {
                        log_warn!("запись, кадр: {e}");
                    }
                }
            }
            Err(e) => log_warn!("запись, захват: {e}"),
        }
        if cursor.load(Ordering::Relaxed) {
            canvas.cursor(region);
        }
        // Frames are stamped on a fixed grid; a late turn skips ahead instead of drifting.
        let due = (elapsed.as_nanos() as i64 / (frame * 100)).max(frames);
        match &sink {
            Sink::Mp4(writer) => {
                if let Err(e) = writer.video(canvas.pixels(), due * frame, frame) {
                    result = Err(e);
                    break;
                }
            }
            Sink::Gif(writer) => writer.frame(canvas.pixels(), Duration::from_nanos((due * frame * 100) as u64)),
        }
        frames = due + 1;
        if let (Some(s), Sink::Mp4(writer)) = (&mut sound, &sink)
            && let Err(e) = s.drain(writer, started.elapsed())
        {
            log_warn!("запись: звук прервался: {e}");
            sound = None;
        }
        if last_tick.is_none_or(|t| elapsed - t >= Duration::from_millis(250)) {
            last_tick = Some(elapsed);
            notify(Event::Tick(elapsed));
        }
        let next = Duration::from_nanos((frames * frame * 100) as u64);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(wait) = next.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let cancelled = cancel.load(Ordering::Relaxed);
    let finished = match sink {
        Sink::Mp4(writer) => {
            if let Some(s) = &mut sound
                && let Err(e) = s.close(&writer, started.elapsed())
            {
                log_warn!("запись: конец звука: {e}");
            }
            let finished = if cancelled { Ok(()) } else { result.and_then(|()| writer.finish()) };
            drop(sound);
            drop(writer);
            unsafe {
                let _ = MFShutdown();
            }
            finished
        }
        Sink::Gif(writer) => writer
            .finish(started.elapsed())
            .map_err(|e| Error::new(windows::Win32::Foundation::E_FAIL, e))
            .or_else(|e| if cancelled { Ok(()) } else { Err(e) }),
    };
    finished?;
    log_info!("запись: {seconds:.1} с → {}", path.display());
    Ok(Finished { path: path.to_path_buf(), preview: canvas.image(), seconds })
}

/// Where the frames go.
enum Sink {
    Mp4(Writer),
    Gif(GifWriter),
}

fn device(adapter: &IDXGIAdapter) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let fail = || Error::from(windows::Win32::Foundation::E_FAIL);
    Ok((device.ok_or_else(fail)?, context.ok_or_else(fail)?))
}

/// The frame in a GDI DIB section: the pixels are read into it, and the cursor is drawn on top.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: u32,
    h: u32,
    hotspot: Option<(isize, POINT)>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Result<Self> {
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    // Negative height: rows top to bottom, as the encoder input is declared.
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(dc, bitmap.into());
            Ok(Self { dc, bitmap, old, bits: bits as *mut u8, w, h, hotspot: None })
        }
    }
    fn len(&self) -> usize {
        (self.w * self.h * 4) as usize
    }
    fn pixels(&self) -> &[u8] {
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts(self.bits, self.len())
        }
    }
    fn read(&mut self, context: &ID3D11DeviceContext, staging: &ID3D11Texture2D) -> Result<()> {
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let row = (self.w * 4) as usize;
            for y in 0..self.h as usize {
                std::ptr::copy_nonoverlapping(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    self.bits.add(y * row),
                    row,
                );
            }
            context.Unmap(staging, 0);
        }
        Ok(())
    }
    fn cursor(&mut self, region: Rect) {
        unsafe {
            let mut info = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
            if GetCursorInfo(&mut info).is_err() || info.flags.0 & CURSOR_SHOWING.0 == 0 || info.hCursor.is_invalid() {
                return;
            }
            let key = info.hCursor.0 as isize;
            let hot = match self.hotspot {
                Some((k, p)) if k == key => p,
                _ => {
                    let mut icon = ICONINFO::default();
                    if GetIconInfo(HICON(info.hCursor.0), &mut icon).is_err() {
                        return;
                    }
                    for b in [icon.hbmMask, icon.hbmColor] {
                        if !b.is_invalid() {
                            let _ = DeleteObject(b.into());
                        }
                    }
                    let p = POINT { x: icon.xHotspot as i32, y: icon.yHotspot as i32 };
                    self.hotspot = Some((key, p));
                    p
                }
            };
            let (x, y) = (info.ptScreenPos.x - region.left - hot.x, info.ptScreenPos.y - region.top - hot.y);
            let _ = DrawIconEx(self.dc, x, y, HICON(info.hCursor.0), 0, 0, 0, None, DI_NORMAL);
        }
    }
    fn image(&self) -> Image {
        let mut bgra = self.pixels().to_vec();
        bgra.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
        Image::new(Rect::new(0, 0, self.w as i32, self.h as i32), bgra)
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Writer {
    sink: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
}

fn media_type(major: &windows::core::GUID, subtype: &windows::core::GUID) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, major)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        Ok(t)
    }
}

impl Writer {
    fn new(path: &std::path::Path, w: u32, h: u32, rate: Option<u32>) -> Result<Self> {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            let sink = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attributes)?;
            let size = ((w as u64) << 32) | h as u64;
            let fps = ((FPS as u64) << 32) | 1;
            let square = (1u64 << 32) | 1;
            let out = media_type(&MFMediaType_Video, &MFVideoFormat_H264)?;
            // About 0.1 bit per pixel per frame: crisp text without huge files.
            let bitrate = ((w * h) as f64 * FPS as f64 * 0.1).clamp(1.5e6, 40e6) as u32;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            out.SetUINT64(&MF_MT_FRAME_RATE, fps)?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, square)?;
            let video = sink.AddStream(&out)?;
            let input = media_type(&MFMediaType_Video, &MFVideoFormat_RGB32)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, fps)?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, square)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
            sink.SetInputMediaType(video, &input, None)?;
            let audio = match rate {
                Some(rate) => {
                    let out = media_type(&MFMediaType_Audio, &MFAudioFormat_AAC)?;
                    out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                    out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                    out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?;
                    let index = sink.AddStream(&out)?;
                    let input = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
                    input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                    input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                    input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
                    input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * 4)?;
                    sink.SetInputMediaType(index, &input, None)?;
                    Some(index)
                }
                None => None,
            };
            sink.BeginWriting()?;
            Ok(Self { sink, video, audio })
        }
    }
    fn write(&self, stream: u32, bytes: &[u8], time: i64, duration: i64) -> Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.sink.WriteSample(stream, &sample)
        }
    }
    fn video(&self, bgra: &[u8], time: i64, duration: i64) -> Result<()> {
        self.write(self.video, bgra, time, duration)
    }
    fn finish(&self) -> Result<()> {
        unsafe { self.sink.Finalize() }
    }
}

/// One sound input read as float stereo: what the speakers play (WASAPI loopback) or the default
/// microphone.
struct Source {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    channels: usize,
    float: bool,
}

impl Source {
    /// What the speakers play, in the engine's own format; its sample rate.
    fn speakers() -> Result<(Self, u32)> {
        unsafe {
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = client.GetMixFormat()?;
            let f = *format;
            let float = f.wFormatTag as u32 == WAVE_FORMAT_IEEE_FLOAT
                || (f.wFormatTag == 0xFFFE && {
                    let ext = *(format as *const WAVEFORMATEXTENSIBLE);
                    let sub = ext.SubFormat;
                    sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                });
            let (rate, channels, bits) = (f.nSamplesPerSec, f.nChannels as usize, f.wBitsPerSample);
            let started = client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 10_000_000, 0, format, None)
                .and_then(|()| client.GetService::<IAudioCaptureClient>())
                .and_then(|capture| client.Start().map(|()| capture));
            CoTaskMemFree(Some(format as *const _));
            let capture = started?;
            // AAC takes 44.1 or 48 kHz; float or 16-bit PCM is what shared mode hands out.
            if !matches!(rate, 44_100 | 48_000) || !(float && bits == 32 || !float && bits == 16) || channels == 0 {
                let _ = client.Stop();
                return Err(Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    format!("формат звука {rate} Гц, {bits} бит, {channels} кан. не поддерживается"),
                ));
            }
            Ok((Self { client, capture, channels, float }, rate))
        }
    }

    /// The default microphone, which Windows converts to float stereo at `rate` for us.
    fn microphone(rate: u32) -> Result<Self> {
        unsafe {
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = devices.GetDefaultAudioEndpoint(eCapture, eConsole)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
                nChannels: 2,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 10_000_000, 0, &format, None)?;
            let capture = client.GetService::<IAudioCaptureClient>()?;
            client.Start()?;
            Ok(Self { client, capture, channels: 2, float: true })
        }
    }

    /// Every packet waiting, appended to `out` as interleaved float stereo.
    fn read(&self, out: &mut Vec<f32>) -> Result<()> {
        unsafe {
            while self.capture.GetNextPacketSize()? != 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                for frame in 0..frames as usize {
                    for c in 0..2 {
                        out.push(if silent {
                            0.0
                        } else {
                            let i = frame * self.channels + c.min(self.channels - 1);
                            if self.float {
                                *(data as *const f32).add(i)
                            } else {
                                *(data as *const i16).add(i) as f32 / 32768.0
                            }
                        });
                    }
                }
                self.capture.ReleaseBuffer(frames)?;
            }
        }
        Ok(())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// `open`, unless it hangs: an antivirus that guards audio capture can hold the call for good.
/// Then the video goes without that sound instead of never starting.
fn in_time<T: 'static>(what: &str, open: impl FnOnce() -> Result<T> + Send + 'static) -> Option<T> {
    /// Made on one thread of the multithreaded apartment, used on another: WASAPI's objects
    /// allow that, though the bindings cannot know it.
    struct Handed<T>(Result<T>);
    unsafe impl<T> Send for Handed<T> {}
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let _ = tx.send(Handed(open()));
    });
    match rx.recv_timeout(Duration::from_secs(3)).map(|h| h.0) {
        Ok(Ok(source)) => Some(source),
        Ok(Err(e)) => {
            log_warn!("запись: {what} недоступен: {e}");
            None
        }
        Err(_) => {
            log_warn!("запись: {what} не ответил за 3 с (его может держать антивирус) — пишу без него");
            None
        }
    }
}

/// Adds the waiting microphone samples to `main`, one for one. The microphone may run a little
/// ahead or behind the speakers' clock: no more than `limit` samples are kept waiting, the oldest
/// go first, and when it falls behind the rest of `main` stays as it is.
fn mix(main: &mut [f32], waiting: &mut std::collections::VecDeque<f32>, limit: usize) {
    let excess = waiting.len().saturating_sub(limit + main.len());
    waiting.drain(..excess);
    let take = main.len().min(waiting.len());
    for (sample, mic) in main.iter_mut().zip(waiting.drain(..take)) {
        *sample += mic;
    }
}

/// The recording's sound track, 16-bit stereo: the speakers (or, without them, the microphone)
/// set the clock, and the microphone, when wanted, is mixed in.
struct Sound {
    main: Source,
    microphone: Option<Source>,
    /// Microphone samples (interleaved stereo) not yet mixed in.
    waiting: std::collections::VecDeque<f32>,
    rate: u32,
    /// Stereo frames handed to the encoder so far: their count is the audio clock.
    written: u64,
}

impl Sound {
    fn start(system_audio: bool, microphone: bool) -> Option<Self> {
        let speakers = if system_audio { in_time("звук компьютера", Source::speakers) } else { None };
        let rate = speakers.as_ref().map_or(48_000, |(_, rate)| *rate);
        let mic = if microphone { in_time("микрофон", move || Source::microphone(rate)) } else { None };
        let (main, microphone) = match (speakers, mic) {
            (Some((speakers, _)), mic) => (speakers, mic),
            (None, Some(mic)) => (mic, None),
            (None, None) => return None,
        };
        Some(Self { main, microphone, waiting: Default::default(), rate, written: 0 })
    }

    fn describe(&self) -> String {
        format!("{} Гц{}", self.rate, if self.microphone.is_some() { ", с микрофоном" } else { "" })
    }

    /// Hands every packet waiting to the encoder; silence where Windows sent none, so sound and
    /// picture stay in step when nothing plays.
    fn drain(&mut self, writer: &Writer, elapsed: Duration) -> Result<()> {
        self.drain_with(writer, elapsed, self.rate as u64 / 10)
    }

    /// At the end: the track runs exactly as long as the picture, never empty.
    fn close(&mut self, writer: &Writer, elapsed: Duration) -> Result<()> {
        let elapsed = elapsed.max(Duration::from_millis(50));
        self.drain_with(writer, elapsed, 0)
    }

    fn drain_with(&mut self, writer: &Writer, elapsed: Duration, slack: u64) -> Result<()> {
        let Some(stream) = writer.audio else { return Ok(()) };
        let mut samples: Vec<f32> = Vec::new();
        self.main.read(&mut samples)?;
        if let Some(mic) = &self.microphone {
            let mut voice = Vec::new();
            match mic.read(&mut voice) {
                Ok(()) => self.waiting.extend(voice),
                Err(e) => {
                    log_warn!("запись: микрофон прервался: {e}");
                    self.microphone = None;
                }
            }
        }
        // Behind the wall clock by more than `slack`: nothing was playing.
        let due = (elapsed.as_secs_f64() * self.rate as f64) as u64;
        let have = self.written + samples.len() as u64 / 2;
        if have + slack < due {
            samples.splice(0..0, std::iter::repeat_n(0.0, ((due - have) * 2) as usize));
        }
        mix(&mut samples, &mut self.waiting, self.rate as usize * 2 * 3 / 10);
        if samples.is_empty() {
            return Ok(());
        }
        let frames = samples.len() as u64 / 2;
        let time = (self.written * 10_000_000 / self.rate as u64) as i64;
        let duration = (frames * 10_000_000 / self.rate as u64) as i64;
        let bytes: Vec<u8> =
            samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        writer.write(stream, &bytes, time, duration)?;
        self.written += frames;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_area_is_trimmed_to_its_monitor_and_to_even_sides() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        assert_eq!(fit(Rect::new(101, 51, 402, 303), monitor), Some(Rect::new(101, 51, 401, 303)));
        assert_eq!(fit(Rect::new(1800, 1000, 2100, 1300), monitor), Some(Rect::new(1800, 1000, 1920, 1080)));
        assert_eq!(fit(Rect::new(10, 10, 20, 20), monitor), None);
    }

    /// A real two-second recording of the top-left corner of the screen, with sound if any.
    #[test]
    fn records_a_short_clip_into_a_playable_mp4() {
        record_clip(false);
    }
    #[test]
    fn disabled_sound_inputs_do_not_open_an_audio_device() {
        assert!(Sound::start(false, false).is_none());
    }

    /// The same with the microphone asked for: mixed in when there is one, and when there is none
    /// (or Windows does not allow it) the video is still recorded.
    #[test]
    fn records_with_the_microphone_or_without_one() {
        record_clip(true);
    }

    #[test]
    fn the_microphone_is_mixed_in_one_for_one_and_never_lags_far_behind() {
        let mut waiting: std::collections::VecDeque<f32> = [0.25, 0.5, 0.25, 0.5].into_iter().collect();
        let mut main = vec![0.5, 0.0, 0.5, 0.0, 0.1, 0.1];
        mix(&mut main, &mut waiting, 100);
        assert_eq!(main, [0.75, 0.5, 0.75, 0.5, 0.1, 0.1], "what has not arrived yet leaves the speakers alone");
        assert!(waiting.is_empty());
        // Far ahead of the clock: only the newest `limit` samples beyond this block are kept.
        let mut waiting: std::collections::VecDeque<f32> = (0..10).map(|i| i as f32).collect();
        let mut main = vec![0.0; 2];
        mix(&mut main, &mut waiting, 4);
        assert_eq!(main, [4.0, 5.0]);
        assert_eq!(waiting.into_iter().collect::<Vec<_>>(), [6.0, 7.0, 8.0, 9.0]);
    }

    fn record_clip(microphone: bool) {
        let path = std::env::temp_dir().join(format!("kadr-record-{}-{microphone}.mp4", std::process::id()));
        let (tx, rx) = std::sync::mpsc::channel();
        let ticks = tx.clone();
        let rec = start_with_audio(Rect::new(0, 0, 320, 240), path.clone(), true, microphone, true, move |e| match e {
            Event::Tick(_) => {
                let _ = ticks.send(None);
            }
            Event::Done(r) => {
                let _ = tx.send(Some(r));
            }
        });
        // Setting up the encoder can take a moment; time counts from the first frame. A failure
        // before it says why.
        if let Some(early) = rx.recv_timeout(Duration::from_secs(20)).expect("started") {
            panic!("ended before the first frame: {:?}", early.err());
        }
        std::thread::sleep(Duration::from_millis(2000));
        rec.stop();
        let done = loop {
            if let Some(r) = rx.recv_timeout(Duration::from_secs(20)).expect("finished") {
                break r.expect("recorded");
            }
        };
        let size = std::fs::metadata(&done.path).map(|m| m.len()).unwrap_or(0);
        assert!(size > 2_000, "file of {size} bytes");
        assert!((1.5..4.0).contains(&done.seconds));
        assert_eq!((done.preview.width(), done.preview.height()), (320, 240));
        // An MP4 starts with an «ftyp» box.
        let head = std::fs::read(&done.path).unwrap();
        assert_eq!(&head[4..8], b"ftyp");
        if let Some(keep) = std::env::var_os("KADR_KEEP_CLIP") {
            let _ = std::fs::copy(&done.path, keep);
        }
        let _ = std::fs::remove_file(&done.path);
    }

    #[test]
    fn records_a_gif_that_loops_for_about_as_long_as_it_ran() {
        let path = std::env::temp_dir().join(format!("kadr-record-{}.gif", std::process::id()));
        let (tx, rx) = std::sync::mpsc::channel();
        let rec = start_with_audio(Rect::new(0, 0, 320, 240), path.clone(), false, false, true, move |e| {
            if let Event::Done(r) = e {
                let _ = tx.send(r);
            }
        });
        std::thread::sleep(Duration::from_millis(1500));
        rec.stop();
        let done = rx.recv_timeout(Duration::from_secs(20)).expect("finished").expect("recorded");
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(std::fs::File::open(&done.path).unwrap()).unwrap();
        assert_eq!((decoder.width(), decoder.height()), (320, 240));
        let mut total = 0u32;
        let mut count = 0;
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            total += frame.delay as u32;
            count += 1;
        }
        assert!(count >= 1);
        // Centiseconds: the animation lasts as long as the recording, give or take a frame.
        assert!((total as f64 / 100.0 - done.seconds).abs() < 0.3, "{total} cs vs {:.2} s", done.seconds);
        let _ = std::fs::remove_file(&done.path);
    }
}
