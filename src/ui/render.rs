//! The D2D/DirectWrite bridge into the composition tree.
//!
//! Composition has no text or bitmap primitives of its own, so tile content is
//! drawn with Direct2D into a `CompositionDrawingSurface` and shown through a
//! `CompositionSurfaceBrush`. That surface is the same kind of object a
//! Windows.Graphics.Capture frame becomes in Milestone 3, so the tile visual
//! tree does not have to change when previews arrive.

use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice, Compositor};
use windows::Win32::Foundation::{HMODULE, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{Interface, Result, w};
use windows_numerics::Matrix3x2;

use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorInterop,
};

use crate::shell::icons::IconPixels;
use crate::{log_info, log_warn};

/// Colours resolved once, so drawing never re-parses config strings.
#[derive(Clone, Copy)]
pub struct TextColors {
    pub title: D2D1_COLOR_F,
    pub detail: D2D1_COLOR_F,
}

/// Everything one tile needs painted. Grouped so callers pass a value rather
/// than a long positional argument list.
pub struct TilePaint<'a> {
    pub width: f32,
    pub height: f32,
    pub label_height: f32,
    pub title: &'a str,
    pub detail: &'a str,
    /// `None` until the shell worker delivers an icon.
    pub icon: Option<&'a IconPixels>,
    pub colors: TextColors,
}

pub struct Renderer {
    graphics: CompositionGraphicsDevice,
    title_format: IDWriteTextFormat,
    detail_format: IDWriteTextFormat,
    header_format: IDWriteTextFormat,
    /// Held so the D2D device outlives every context it hands out.
    _d2d_device: ID2D1Device,
    _d3d_device: ID3D11Device,
}

impl Renderer {
    pub fn new(compositor: &Compositor) -> Result<Renderer> {
        let d3d_device = create_d3d_device()?;
        let dxgi: IDXGIDevice = d3d_device.cast()?;

        // SAFETY: single-threaded factory, used only from the UI thread.
        let factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        // SAFETY: dxgi comes from the D3D device created just above.
        let d2d_device = unsafe { factory.CreateDevice(&dxgi)? };

        let interop: ICompositorInterop = compositor.cast()?;
        // SAFETY: d2d_device is a live rendering device the compositor accepts.
        let graphics = unsafe { interop.CreateGraphicsDevice(&d2d_device)? };

        // SAFETY: the shared factory is reference counted by DirectWrite.
        let dwrite: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        let title_format =
            text_format(&dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let detail_format =
            text_format(&dwrite, DWRITE_FONT_WEIGHT_NORMAL, 11.0, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        // Headers read as labels, so they sit left-aligned against the padding.
        let header_format =
            text_format(&dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 12.0, DWRITE_TEXT_ALIGNMENT_LEADING)?;

        Ok(Renderer {
            graphics,
            title_format,
            detail_format,
            header_format,
            _d2d_device: d2d_device,
            _d3d_device: d3d_device,
        })
    }

    pub fn create_surface(&self, width: f32, height: f32) -> Result<CompositionDrawingSurface> {
        self.graphics.CreateDrawingSurface(
            Size { Width: width, Height: height },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
    }

    /// Paint one tile's content: icon above, title and detail below.
    ///
    /// `icon` is `None` until the shell worker delivers one, so this is called
    /// again for the same surface when it arrives.
    pub fn draw_tile(
        &self,
        surface: &CompositionDrawingSurface,
        paint: TilePaint<'_>,
    ) -> Result<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;

        // SAFETY: BeginDraw hands back a context valid until EndDraw. Every path
        // out of the draw block below calls EndDraw exactly once.
        let (context, offset): (ID2D1DeviceContext, POINT) = unsafe {
            let mut offset = POINT::default();
            let context = interop.BeginDraw(None, &mut offset)?;
            (context, offset)
        };

        let result = self.paint(&context, offset, paint);

        // SAFETY: pairs with the BeginDraw above; must run even if paint failed,
        // or the surface stays locked forever.
        unsafe {
            interop.EndDraw()?;
        }
        result
    }

    fn paint(
        &self,
        context: &ID2D1DeviceContext,
        offset: POINT,
        paint: TilePaint<'_>,
    ) -> Result<()> {
        let TilePaint { width, height, label_height, title, detail, icon, colors } = paint;
        // The surface may live inside a shared atlas, so everything is drawn
        // relative to the offset BeginDraw reported.
        let dx = offset.x as f32;
        let dy = offset.y as f32;

        // SAFETY: the context is live between BeginDraw and EndDraw, and every
        // resource below is created from it.
        unsafe {
            context.SetTransform(&Matrix3x2::translation(dx, dy));
            context.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));

            let icon_area_h = (height - label_height).max(0.0);
            if let Some(icon) = icon
                && let Ok(bitmap) = create_bitmap(context, icon)
            {
                // Fit inside the icon area without upscaling past the source.
                let max = (icon_area_h * 0.6).min(width * 0.5);
                let side = max.min(icon.width.max(icon.height) as f32).max(1.0);
                let left = (width - side) / 2.0;
                let top = (icon_area_h - side) / 2.0;
                context.DrawBitmap(
                    &bitmap,
                    Some(&D2D_RECT_F {
                        left,
                        top,
                        right: left + side,
                        bottom: top + side,
                    }),
                    1.0,
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                );
            }

            let pad = 8.0;
            // With no detail line the title gets the whole label strip, which
            // keeps it vertically centred instead of riding high.
            let title_h = if detail.is_empty() {
                label_height.max(1.0)
            } else {
                (label_height * 0.58).max(1.0)
            };
            self.draw_text(
                context,
                title,
                &self.title_format,
                D2D_RECT_F {
                    left: pad,
                    top: icon_area_h,
                    right: width - pad,
                    bottom: icon_area_h + title_h,
                },
                colors.title,
            )?;
            self.draw_text(
                context,
                detail,
                &self.detail_format,
                D2D_RECT_F {
                    left: pad,
                    top: icon_area_h + title_h,
                    right: width - pad,
                    bottom: height,
                },
                colors.detail,
            )?;
        }
        Ok(())
    }

    /// A section header: one line of text on a transparent surface.
    pub fn draw_header(
        &self,
        surface: &CompositionDrawingSurface,
        width: f32,
        height: f32,
        title: &str,
        color: D2D1_COLOR_F,
    ) -> Result<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;

        // SAFETY: BeginDraw hands back a context valid until EndDraw, which the
        // matching call below always runs.
        let (context, offset): (ID2D1DeviceContext, POINT) = unsafe {
            let mut offset = POINT::default();
            let context = interop.BeginDraw(None, &mut offset)?;
            (context, offset)
        };

        // SAFETY: the context is live until EndDraw.
        let result = unsafe {
            context.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
            context.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            self.draw_text(
                &context,
                title,
                &self.header_format,
                D2D_RECT_F { left: 0.0, top: 0.0, right: width, bottom: height },
                color,
            )
        };

        // SAFETY: pairs with BeginDraw; must run even on failure or the surface
        // stays locked.
        unsafe {
            interop.EndDraw()?;
        }
        result
    }

    unsafe fn draw_text(
        &self,
        context: &ID2D1DeviceContext,
        text: &str,
        format: &IDWriteTextFormat,
        rect: D2D_RECT_F,
        color: D2D1_COLOR_F,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: caller holds a live device context; the brush and the string
        // both outlive the DrawText call.
        unsafe {
            let brush = context.CreateSolidColorBrush(&color, None)?;
            context.DrawText(
                &utf16,
                format,
                &rect,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }
}

/// Icons arrive as premultiplied BGRA, which is what the surface expects.
unsafe fn create_bitmap(context: &ID2D1DeviceContext, icon: &IconPixels) -> Result<ID2D1Bitmap1> {
    let properties = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: core::mem::ManuallyDrop::new(None),
    };
    // SAFETY: the pixel buffer is at least width * height * 4 bytes, which
    // IconPixels guarantees on construction, and it outlives this call.
    unsafe {
        context.CreateBitmap(
            D2D_SIZE_U { width: icon.width, height: icon.height },
            Some(icon.bgra.as_ptr() as *const core::ffi::c_void),
            icon.width * 4,
            &properties,
        )
    }
}

fn text_format(
    dwrite: &IDWriteFactory,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    size: f32,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    // SAFETY: all arguments are owned by the caller for the duration.
    let format = unsafe {
        dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?
    };
    // SAFETY: configuring a format we just created.
    unsafe {
        format.SetTextAlignment(alignment)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;

        // Window titles are long and uncontrolled; ellipsize rather than clip.
        let sign = dwrite.CreateEllipsisTrimmingSign(&format)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        format.SetTrimming(&trimming, &sign)?;
    }
    Ok(format)
}

/// Hardware first, WARP as a fallback. A machine that cannot create either has
/// no working composition stack at all, so the error propagates.
fn create_d3d_device() -> Result<ID3D11Device> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device = None;
        // SAFETY: standard device creation; BGRA support is required for D2D
        // interop, and the out-param is a plain Option<ID3D11Device>.
        let hr = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        match (hr, device) {
            (Ok(()), Some(device)) => {
                if driver == D3D_DRIVER_TYPE_WARP {
                    log_warn!("no hardware D3D11 device; falling back to WARP (software)");
                } else {
                    log_info!("D3D11 hardware device created");
                }
                return Ok(device);
            }
            _ => continue,
        }
    }
    Err(windows::core::Error::from_thread())
}

/// Convenience for turning a config colour into the D2D form.
pub fn d2d_color(spec: &str) -> D2D1_COLOR_F {
    let (a, r, g, b) = crate::config::parse_color(spec);
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
}

/// Kept for the update-rect form of BeginDraw once previews land in Milestone 3.
#[allow(dead_code)]
pub type UpdateRect = RECT;
