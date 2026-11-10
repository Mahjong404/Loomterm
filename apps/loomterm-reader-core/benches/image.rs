use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
use loomterm_reader_core::{load_local_image, Cancellation, ImageLimits, ImageReference};
use std::{fs, path::Path, time::Instant};

fn measure(size: u32) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/image-bench-files")
        .join(std::process::id().to_string());
    fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{size}.png"));
    let pixels = vec![127u8; size as usize * size as usize * 4];
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(&pixels, size, size, ExtendedColorType::Rgba8)
        .unwrap();
    fs::write(&path, &encoded).unwrap();
    let reference = ImageReference::Local { path: path.clone() };
    let token = Cancellation::default();
    let mut times = Vec::with_capacity(20);
    for _ in 0..20 {
        let start = Instant::now();
        let result = load_local_image(&reference, ImageLimits::default(), &token).unwrap();
        times.push(start.elapsed().as_micros());
        assert_eq!(result.rgba.len(), pixels.len());
    }
    times.sort_unstable();
    println!(
        "{{\"size\":{size},\"encoded_bytes\":{},\"samples\":20,\"median_us\":{},\"p95_us\":{}}}",
        encoded.len(),
        times[10],
        times[18]
    );
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&root).unwrap();
}

fn main() {
    measure(256);
    measure(1024);
}
