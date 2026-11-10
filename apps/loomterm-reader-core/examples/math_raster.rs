//! Generate bounded offline raster evidence; this is not a Reader window.
use loomterm_reader_core::{math::*, Cancellation};
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("supply an explicit output directory")?,
    );
    fs::create_dir_all(&directory)?;
    for (name, source, display) in [
        ("inline", "E=mc^2", false),
        ("fraction", r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}", true),
        ("matrix", r"\begin{pmatrix}a&b\\c&d\end{pmatrix}", true),
        (
            "aligned",
            r"\begin{aligned}a&=b+c\\d&=\sqrt{x}\end{aligned}",
            true,
        ),
    ] {
        let result = render_math(
            source,
            display,
            MathOptions::default(),
            MathLimits::default(),
            &Cancellation::default(),
        )?;
        image::save_buffer_with_format(
            directory.join(format!("{name}.png")),
            &result.image.rgba,
            result.image.width,
            result.image.height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )?;
        println!(
            "{name}: {}x{} RGBA, baseline {}",
            result.image.width, result.image.height, result.baseline_px
        );
    }
    Ok(())
}
