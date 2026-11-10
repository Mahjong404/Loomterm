//! Opt-in offline formula raster prototype. No document commands, font paths or I/O.
//! Run on one bounded worker. Cancellation cannot interrupt third-party typesetting.
use crate::{Cancellation, DecodedImage};
use ab_glyph::Font;
use ratex_types::{color::Color, display_item::DisplayItem, math_style::MathStyle};

#[derive(Clone, Copy, Debug)]
pub struct MathLimits {
    pub source_bytes: usize,
    pub tokens: usize,
    pub depth: usize,
    pub draw_items: usize,
    pub pixels: u64,
    pub dimension: u32,
}
impl Default for MathLimits {
    fn default() -> Self {
        Self {
            source_bytes: 2048,
            tokens: 512,
            depth: 24,
            draw_items: 4096,
            pixels: 512 * 1024,
            dimension: 2048,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MathOptions {
    /// Physical pixels per em, including host DPI. No system font lookup.
    pub font_px: u16,
    pub foreground: [u8; 4],
}
impl Default for MathOptions {
    fn default() -> Self {
        Self {
            font_px: 24,
            foreground: [24, 24, 24, 255],
        }
    }
}
#[derive(Debug)]
pub struct RenderedMath {
    pub image: DecodedImage,
    /// Distance from the image's top edge to the text baseline, in pixels.
    pub baseline_px: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathError {
    InvalidLimits,
    SourceBudget,
    Complexity,
    Unsupported,
    InvalidFormula,
    OutputBudget,
    Cancelled,
    Raster,
}
impl std::fmt::Display for MathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MathError {}

fn check_cancel(cancel: &Cancellation) -> Result<(), MathError> {
    if cancel.is_cancelled() {
        Err(MathError::Cancelled)
    } else {
        Ok(())
    }
}

// Deliberately smaller than RaTeX: no macro definitions, HTML, URLs, file commands,
// arbitrary dimensions, chemistry, font discovery or user-defined expansion.
const COMMANDS: &[&str] = &[
    "frac",
    "dfrac",
    "tfrac",
    "sqrt",
    "binom",
    "sum",
    "prod",
    "int",
    "iint",
    "lim",
    "sin",
    "cos",
    "tan",
    "log",
    "ln",
    "exp",
    "infty",
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "theta",
    "lambda",
    "mu",
    "pi",
    "rho",
    "sigma",
    "phi",
    "omega",
    "Gamma",
    "Delta",
    "Theta",
    "Lambda",
    "Pi",
    "Sigma",
    "Phi",
    "Omega",
    "pm",
    "mp",
    "times",
    "cdot",
    "div",
    "leq",
    "geq",
    "neq",
    "approx",
    "equiv",
    "to",
    "rightarrow",
    "leftarrow",
    "in",
    "notin",
    "subset",
    "cup",
    "cap",
    "partial",
    "nabla",
    "forall",
    "exists",
    "ldots",
    "cdots",
    "left",
    "right",
    "begin",
    "end",
    "text",
    "mathrm",
    "mathbf",
    "mathit",
    "mathbb",
    "overline",
    "underline",
    "vec",
    "hat",
    "quad",
    "qquad",
    "limits",
    "nolimits",
];
fn preflight(source: &str, limits: MathLimits) -> Result<(), MathError> {
    if source.len() > limits.source_bytes {
        return Err(MathError::SourceBudget);
    }
    if source.trim().is_empty() {
        return Err(MathError::InvalidFormula);
    }
    // ASCII TeX notation only. Greek symbols use commands; reject system-font
    // fallback before it can inspect environment or load platform font files.
    if !source.is_ascii() {
        return Err(MathError::Unsupported);
    }
    let bytes = source.as_bytes();
    let (mut at, mut depth, mut tokens) = (0, 0usize, 0usize);
    while at < bytes.len() {
        let b = bytes[at];
        at += 1;
        tokens += 1;
        if tokens > limits.tokens {
            return Err(MathError::Complexity);
        }
        match b {
            b'\\' => {
                let start = at;
                while at < bytes.len() && bytes[at].is_ascii_alphabetic() {
                    at += 1;
                }
                if at == start {
                    if at == bytes.len() || !b"\\{},;! |".contains(&bytes[at]) {
                        return Err(MathError::Unsupported);
                    }
                    at += 1;
                } else if !COMMANDS.contains(&&source[start..at]) {
                    return Err(MathError::Unsupported);
                }
            }
            b'{' => {
                depth += 1;
                if depth > limits.depth {
                    return Err(MathError::Complexity);
                }
            }
            b'}' => {
                depth = depth.checked_sub(1).ok_or(MathError::InvalidFormula)?;
            }
            b'%' | b'#' | b'$' => return Err(MathError::Unsupported),
            b if b.is_ascii_control() && !b.is_ascii_whitespace() => {
                return Err(MathError::Unsupported)
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(MathError::InvalidFormula);
    }
    Ok(())
}

/// Typeset one inline/display math node using embedded OFL KaTeX fonts.
/// Invalid/unsupported input returns a bounded error; the host paints the original
/// source with an error indication. No expression result is installed automatically.
///
/// Pixel and work limits are preflight checks, not a hard CPU or peak-RSS quota.
/// Upstream process-global font/outline/glyph caches outlive individual Readers;
/// this opt-in prototype does not claim per-window release of those caches.
pub fn render_math(
    source: &str,
    display: bool,
    options: MathOptions,
    limits: MathLimits,
    cancel: &Cancellation,
) -> Result<RenderedMath, MathError> {
    if limits.source_bytes > 4096
        || limits.tokens > 1024
        || limits.depth > 32
        || limits.draw_items > 8192
        || limits.pixels > 1024 * 1024
        || limits.dimension > 4096
        || options.font_px == 0
        || options.font_px > 128
    {
        return Err(MathError::InvalidLimits);
    }
    check_cancel(cancel)?;
    preflight(source, limits)?;
    let ast = ratex_parser::parse(source).map_err(|_| MathError::InvalidFormula)?;
    check_cancel(cancel)?;
    let [r, g, b, a] = options.foreground;
    let color = Color::new(
        r as f32 / 255.,
        g as f32 / 255.,
        b as f32 / 255.,
        a as f32 / 255.,
    );
    let layout_options = ratex_layout::LayoutOptions::default()
        .with_style(if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        })
        .with_color(color);
    let layout = ratex_layout::layout(&ast, &layout_options);
    ratex_layout::validate_layout(&layout).map_err(|_| MathError::OutputBudget)?;
    check_cancel(cancel)?;
    let mut list = ratex_layout::to_display_list(&layout);
    if list.items.len() > limits.draw_items {
        return Err(MathError::OutputBudget);
    }
    let mut path_commands = 0usize;
    for item in &list.items {
        match item {
            DisplayItem::GlyphPath {
                font, char_code, ..
            } => {
                let id = ratex_font::FontId::parse(font).ok_or(MathError::Unsupported)?;
                // Refuse any glyph requiring fallback, not just explicit CJK nodes.
                if matches!(
                    id,
                    ratex_font::FontId::CjkRegular
                        | ratex_font::FontId::CjkFallback
                        | ratex_font::FontId::EmojiFallback
                ) || ratex_font::get_char_metrics(id, *char_code).is_none()
                {
                    return Err(MathError::Unsupported);
                }
            }
            DisplayItem::Path { commands, .. } => {
                path_commands += commands.len();
            }
            _ => {}
        }
    }
    if path_commands > limits.draw_items {
        return Err(MathError::OutputBudget);
    }
    let em = f32::from(options.font_px);
    let width = (list.width as f32 * em + 4.).ceil().max(1.);
    let height = (list.total_height() as f32 * em + 4.).ceil().max(1.);
    if !width.is_finite()
        || !height.is_finite()
        || width > limits.dimension as f32
        || height > limits.dimension as f32
        || width as u64 * height as u64 > limits.pixels
    {
        return Err(MathError::OutputBudget);
    }
    check_cancel(cancel)?;
    // Whitespace has advance but no outline. Do not let the renderer's missing-
    // outline fallback discover system fonts for an intentionally blank glyph.
    list.items
        .retain(|item| !matches!(item, DisplayItem::GlyphPath { char_code: 32, .. }));
    let fonts = ratex_font_loader::load_fonts_for_items_parsed("", &list.items)
        .map_err(|_| MathError::Raster)?;
    for item in &list.items {
        if let DisplayItem::GlyphPath {
            font, char_code, ..
        } = item
        {
            let id = ratex_font::FontId::parse(font).ok_or(MathError::Unsupported)?;
            let face = fonts.get(&id).ok_or(MathError::Raster)?;
            let glyph = face.glyph_id(ratex_font::katex_ttf_glyph_char(id, *char_code));
            if glyph.0 == 0 || face.outline(glyph).is_none() {
                return Err(MathError::Unsupported);
            }
        }
    }
    check_cancel(cancel)?;
    let png = ratex_render::render_to_png(
        &list,
        &ratex_render::RenderOptions {
            font_size: em,
            padding: 2.,
            device_pixel_ratio: 1.,
            background_color: Color::new(0., 0., 0., 0.),
            font_dir: String::new(),
        },
    )
    .map_err(|_| MathError::Raster)?;
    check_cancel(cancel)?;
    let image = ::image::load_from_memory_with_format(&png, ::image::ImageFormat::Png)
        .map_err(|_| MathError::Raster)?
        .into_rgba8();
    if image.width() != width as u32 || image.height() != height as u32 {
        return Err(MathError::Raster);
    }
    check_cancel(cancel)?;
    Ok(RenderedMath {
        baseline_px: (list.height as f32 * em + 2.).clamp(0., height),
        image: DecodedImage {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        },
    })
}
