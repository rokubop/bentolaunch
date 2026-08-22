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
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Bitmap1,
    ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::{Interface, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

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

/// The figure on a tile that has no icon to fetch.
///
/// Drawn rather than set in a glyph, because the whole point of these is to be
/// a picture of the shape the window ends up in. No font has that, and the
/// nearest geometric characters are a guess at what the UI font carries.
#[derive(Clone, Copy)]
pub enum Mark {
    /// A screen outline with the destination filled, as fractions of it.
    Half { left: f32, top: f32, right: f32, bottom: f32 },
    /// Two screens side by side, the destination filled.
    Screen { second: bool },
    /// On or off, read the way a radio button is.
    Latch { on: bool },
    /// One more of the thing this box is full of.
    Plus,
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
    /// Drawn in the icon's place on tiles that are an action rather than a
    /// thing. Never set on a tile that has an icon coming.
    pub mark: Option<Mark>,
    pub colors: TextColors,
}

/// Everything one option square needs painted. Grouped for the same reason
/// `TilePaint` is: seven of these went positional and the eighth was one too
/// many to read at the call site.
pub struct OptionPaint<'a> {
    pub width: f32,
    pub height: f32,
    /// The big mark. Ignored when there is an icon: they occupy the same band,
    /// and the glyph is what stands in until one arrives.
    pub glyph: &'a str,
    pub label: &'a str,
    pub colors: TextColors,
    pub icon: Option<&'a IconPixels>,
}

/// Wide enough to read as a screen rather than a box.
const SCREEN_ASPECT: f32 = 0.625;
const MARK_STROKE: f32 = 1.5;
const MARK_RADIUS: f32 = 2.0;
/// A window sits inside its screen, so the fill stops short of the frame. This
/// is what stops "half of one screen" reading as "one of two screens" at the
/// size these are actually drawn.
const MARK_INSET: f32 = 2.0;
/// A pair of screens is wider and shorter than one screen, and the gap between
/// them is wide enough to be a gap rather than a seam. Fractions of the single
/// screen's width, so the two marks differ in outline before you read either.
const PAIR_WIDTH: f32 = 1.24;
const PAIR_HEIGHT: f32 = 0.52;
const PAIR_GAP: f32 = 0.2;

/// Reads as a search box without needing a border or a caret.
const SEARCH_GLYPH: &str = "\u{E721}";

pub struct Renderer {
    graphics: CompositionGraphicsDevice,
    title_format: IDWriteTextFormat,
    detail_format: IDWriteTextFormat,
    header_format: IDWriteTextFormat,
    /// The filter strip sizes its text per call. See `draw_search`.
    dwrite: IDWriteFactory,
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
            text_format(&dwrite, DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.0, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        Ok(Renderer {
            graphics,
            title_format,
            detail_format,
            header_format,
            dwrite,
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
        let TilePaint { width, height, label_height, title, detail, icon, mark, colors } = paint;
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
            } else if let Some(mark) = mark {
                self.draw_mark(context, mark, width, icon_area_h, colors)?;
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

    /// One option tile in edit mode: a big mark, and what it does underneath.
    ///
    /// Same footprint as an app tile. The controls in this app are aimed at,
    /// sometimes by gaze, so an option has to be as easy to hit as the things
    /// it sits among.
    pub fn draw_option(
        &self,
        surface: &CompositionDrawingSurface,
        paint: OptionPaint<'_>,
    ) -> Result<()> {
        let OptionPaint { width, height, glyph, label, colors, icon } = paint;
        let glyph_format = text_format(
            &self.dwrite,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            (height * 0.30).clamp(14.0, 48.0),
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?;
        let label_format = text_format(
            &self.dwrite,
            DWRITE_FONT_WEIGHT_NORMAL,
            (height * 0.13).clamp(10.0, 20.0),
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?;
        let split = height * 0.62;

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
            // A real icon in the glyph's place when the caller has one. The
            // glyph is the stand-in: an option that is a picture of an app
            // should show that app, not a symbol standing for it.
            let top = height * 0.16;
            let band = split - top;
            match icon.and_then(|icon| Some((icon, create_bitmap(&context, icon).ok()?))) {
                Some((icon, bitmap)) => {
                    // Same rule as a tile's: fit the band, never upscale past
                    // the source, which would only blur it.
                    let side = band
                        .min(width * 0.5)
                        .min(icon.width.max(icon.height) as f32)
                        .max(1.0);
                    let left = (width - side) / 2.0;
                    let top = top + (band - side) / 2.0;
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
                None => self.draw_text(
                    &context,
                    glyph,
                    &glyph_format,
                    D2D_RECT_F { left: 0.0, top, right: width, bottom: split },
                    colors.title,
                )?,
            }
            self.draw_text(
                &context,
                label,
                &label_format,
                D2D_RECT_F { left: 0.0, top: split, right: width, bottom: height },
                colors.detail,
            )
        };

        // SAFETY: pairs with BeginDraw; must run even on failure or the surface
        // stays locked.
        unsafe {
            interop.EndDraw()?;
        }
        result
    }

    /// Search glyph, the query, and how much of the grid survived it.    /// Search glyph, the query, and how much of the grid survived it.
    ///
    /// The count matters: without it, matching nothing and matching everything
    /// both look like an empty grid.
    ///
    /// Sized from `height`, not constants. The user sets that height and DPI
    /// then multiplies it, so fixed point sizes would strand small text in a
    /// tall strip. Built per call, which is once per keystroke against sixty
    /// tile repaints.
    pub fn draw_search(
        &self,
        surface: &CompositionDrawingSurface,
        width: f32,
        height: f32,
        query: &str,
        count: &str,
        colors: TextColors,
    ) -> Result<()> {
        // Ratios are the design. The clamps only catch an absurd height.
        let query_format = text_format(
            &self.dwrite,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            (height * 0.46).clamp(11.0, 64.0),
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?;
        let count_format = text_format(
            &self.dwrite,
            DWRITE_FONT_WEIGHT_NORMAL,
            (height * 0.30).clamp(9.0, 36.0),
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        )?;
        let glyph_format = font_format(
            &self.dwrite,
            w!("Segoe MDL2 Assets"),
            DWRITE_FONT_WEIGHT_NORMAL,
            (height * 0.38).clamp(10.0, 48.0),
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?;

        // The query takes what is left and ellipsizes inside it.
        let glyph_w = (height * 0.78).clamp(16.0, 104.0).min(width);
        let count_w = (height * 2.6).clamp(70.0, 340.0);
        let text_right = (width - count_w).max(glyph_w);

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
                SEARCH_GLYPH,
                &glyph_format,
                D2D_RECT_F { left: 0.0, top: 0.0, right: glyph_w, bottom: height },
                colors.detail,
            )
            .and_then(|()| {
                self.draw_text(
                    &context,
                    query,
                    &query_format,
                    D2D_RECT_F { left: glyph_w, top: 0.0, right: text_right, bottom: height },
                    colors.title,
                )
            })
            .and_then(|()| {
                self.draw_text(
                    &context,
                    count,
                    &count_format,
                    D2D_RECT_F { left: text_right, top: 0.0, right: width, bottom: height },
                    colors.detail,
                )
            })
        };

        // SAFETY: pairs with BeginDraw; must run even on failure.
        unsafe {
            interop.EndDraw()?;
        }
        result
    }


    /// Sized off the same numbers the icon block uses, so a bar of these lines
    /// up with a row of app tiles.
    unsafe fn draw_mark(
        &self,
        context: &ID2D1DeviceContext,
        mark: Mark,
        width: f32,
        icon_area_h: f32,
        colors: TextColors,
    ) -> Result<()> {
        let side = (icon_area_h * 0.6).min(width * 0.5).max(8.0);
        let (w, h) = match mark {
            Mark::Screen { .. } => (side * PAIR_WIDTH, side * PAIR_HEIGHT),
            // A plus has no long axis, so it gets a square to sit in. The latch
            // keeps the screen box every other mark uses: sized off its own
            // shape it came out bigger than the marks it sits beside.
            Mark::Plus => (side, side),
            Mark::Half { .. } | Mark::Latch { .. } => (side, side * SCREEN_ASPECT),
        };
        let left = (width - w) / 2.0;
        let top = (icon_area_h - h) / 2.0;

        // SAFETY: the caller holds a live device context, and both brushes
        // outlive every draw below.
        unsafe {
            let line = context.CreateSolidColorBrush(&colors.detail, None)?;
            let fill = context.CreateSolidColorBrush(&colors.title, None)?;

            let outline = |x: f32, y: f32, w: f32, h: f32| D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h },
                radiusX: MARK_RADIUS,
                radiusY: MARK_RADIUS,
            };

            match mark {
                Mark::Half { left: x0, top: y0, right: x1, bottom: y1 } => {
                    let inset = |a: f32, b: f32| {
                        if b - a > MARK_INSET * 2.0 { (a + MARK_INSET, b - MARK_INSET) } else { (a, b) }
                    };
                    let (fl, fr) = inset(left + w * x0, left + w * x1);
                    let (ft, fb) = inset(top + h * y0, top + h * y1);
                    context.FillRectangle(
                        &D2D_RECT_F { left: fl, top: ft, right: fr, bottom: fb },
                        &fill,
                    );
                    context.DrawRoundedRectangle(&outline(left, top, w, h), &line, MARK_STROKE, None);
                }
                Mark::Screen { second } => {
                    let gap = w * PAIR_GAP;
                    let each = (w - gap) / 2.0;
                    let lit = if second { left + each + gap } else { left };
                    context.FillRoundedRectangle(&outline(lit, top, each, h), &fill);
                    for x in [left, left + each + gap] {
                        context.DrawRoundedRectangle(
                            &outline(x, top, each, h),
                            &line,
                            MARK_STROKE,
                            None,
                        );
                    }
                }
                Mark::Plus => {
                    let arm = w * 0.42;
                    let mid = (left + w / 2.0, top + h / 2.0);
                    let bar = MARK_STROKE * 1.4;
                    context.FillRectangle(
                        &D2D_RECT_F {
                            left: mid.0 - arm,
                            top: mid.1 - bar / 2.0,
                            right: mid.0 + arm,
                            bottom: mid.1 + bar / 2.0,
                        },
                        &fill,
                    );
                    context.FillRectangle(
                        &D2D_RECT_F {
                            left: mid.0 - bar / 2.0,
                            top: mid.1 - arm,
                            right: mid.0 + bar / 2.0,
                            bottom: mid.1 + arm,
                        },
                        &fill,
                    );
                }
                Mark::Latch { on } => {
                    let radius = h / 2.0;
                    let centre = Vector2 { X: width / 2.0, Y: top + radius };
                    let ring = D2D1_ELLIPSE { point: centre, radiusX: radius, radiusY: radius };
                    context.DrawEllipse(&ring, &line, MARK_STROKE, None);
                    if on {
                        let dot = D2D1_ELLIPSE {
                            point: centre,
                            radiusX: radius * 0.5,
                            radiusY: radius * 0.5,
                        };
                        context.FillEllipse(&dot, &fill);
                    }
                }
            }
        }
        Ok(())
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

/// Windows 11's own UI face. DirectWrite falls back to Segoe UI on a machine
/// that somehow lacks it.
const UI_FONT: windows::core::PCWSTR = w!("Segoe UI Variable Text");

fn text_format(
    dwrite: &IDWriteFactory,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    size: f32,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    font_format(dwrite, UI_FONT, weight, size, alignment)
}

fn font_format(
    dwrite: &IDWriteFactory,
    family: windows::core::PCWSTR,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    size: f32,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    // SAFETY: all arguments are owned by the caller for the duration.
    let format = unsafe {
        dwrite.CreateTextFormat(
            family,
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
