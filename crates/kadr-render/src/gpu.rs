//! GPU-контекст на каждый адаптер, к которому подключены мониторы: D3D11 + Direct2D + DirectComposition.
//! Кадр монитора живёт на «своём» адаптере, поэтому копий между GPU нет.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWriteCreateFactory,
    IDWriteFactory, IDWriteFontCollection, IDWriteTypography,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter1, IDXGIDevice1, IDXGIFactory2};
use windows::core::{BOOL, Error, Interface, PCWSTR, Result};

use kadr_win::wide;

use crate::theme;

/// Фабрики, общие для всех адаптеров.
pub struct Shared {
    pub(crate) blur: std::cell::RefCell<Option<crate::annotations::CpuBlur>>,
    pub d2d: ID2D1Factory1,
    pub dwrite: IDWriteFactory,
    /// UTF-16 с нулём на конце — готово для CreateTextFormat.
    pub ui_family: Vec<u16>,
    /// Табличные цифры для всего, что показывает числа.
    pub tabular: IDWriteTypography,
}

impl Shared {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let ui_family = pick_family(&dwrite, theme::UI_FONTS);
            let tabular = dwrite.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            Ok(Self { d2d, dwrite, ui_family, tabular, blur: Default::default() })
        }
    }
    pub fn clear_annotation_cache(&self) {
        *self.blur.borrow_mut() = None;
    }
}

pub struct Gpu {
    pub name: String,
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub factory: IDXGIFactory2,
    pub d2d: ID2D1DeviceContext,
    pub dcomp: IDCompositionDevice,
    /// Одна кисть на контекст: цвет меняется перед каждой фигурой.
    pub brush: ID2D1SolidColorBrush,
    _d2d_device: ID2D1Device,
}

impl Gpu {
    pub fn new(shared: &Shared, adapter: &IDXGIAdapter1, name: String) -> Result<Self> {
        unsafe {
            let levels =
                [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.ok_or_else(|| Error::from(E_FAIL))?;
            let context = context.ok_or_else(|| Error::from(E_FAIL))?;
            let dxgi: IDXGIDevice1 = device.cast()?;
            // Не копить кадры в очереди: на экран всегда идёт самый свежий.
            dxgi.SetMaximumFrameLatency(1)?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let d2d_device = shared.d2d.CreateDevice(&dxgi)?;
            let d2d = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            // ClearType даёт цветную кайму поверх произвольного скрина — только оттенки серого.
            d2d.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let brush = d2d.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)?;
            Ok(Self { name, device, context, factory, d2d, dcomp, brush, _d2d_device: d2d_device })
        }
    }
}

/// Свойства BGRA-битмапа в пикселях (96 DPI: одна единица D2D = один физический пиксель).
pub fn bitmap_props(options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: ManuallyDrop::new(None),
    }
}

/// Первое установленное семейство из списка; иначе последнее (оно есть в любой Windows).
fn pick_family(dwrite: &IDWriteFactory, names: &[&str]) -> Vec<u16> {
    let mut collection: Option<IDWriteFontCollection> = None;
    unsafe {
        let _ = dwrite.GetSystemFontCollection(&mut collection, false);
    }
    if let Some(collection) = collection {
        for name in names {
            let w = wide(name);
            let (mut index, mut exists) = (0u32, BOOL(0));
            let found = unsafe { collection.FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists) };
            if found.is_ok() && exists.as_bool() {
                return w;
            }
        }
    }
    wide(names[names.len() - 1])
}
