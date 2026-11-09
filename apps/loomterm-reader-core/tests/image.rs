use image::{
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    ExtendedColorType, ImageEncoder,
};
use loomterm_reader_core::view::{LogicalSize, ReaderView, ViewApply, ViewLimits, Viewport};
use loomterm_reader_core::{
    load_local_image, BlockedImage, Cancellation, ImageLimits, ImageLoadError, ImageReference,
    Limits, NodeKind, ReaderError,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-files")
            .join(format!(
                "image-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn image(&self, name: &str, bytes: &[u8]) -> ImageReference {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        ImageReference::Local { path }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            &[255, 0, 0, 255, 0, 128, 255, 64],
            2,
            1,
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    bytes
}

#[test]
fn local_png_decodes_as_owned_top_down_rgba_and_releases_file() {
    let fixture = Fixture::new();
    let reference = fixture.image("图像.png", &png());
    let result =
        load_local_image(&reference, ImageLimits::default(), &Cancellation::default()).unwrap();
    assert_eq!(
        (result.width, result.height, result.stride_bytes()),
        (2, 1, 8)
    );
    assert_eq!(result.rgba, [255, 0, 0, 255, 0, 128, 255, 64]);
    drop(result);
    let ImageReference::Local { path } = reference else {
        unreachable!()
    };
    fs::remove_file(path).unwrap();
}

#[test]
fn jpeg_and_webp_are_content_sniffed_without_using_the_extension() {
    let fixture = Fixture::new();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 95)
        .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
        .unwrap();
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp)
        .write_image(&[0, 128, 255, 64], 1, 1, ExtendedColorType::Rgba8)
        .unwrap();
    let token = Cancellation::default();
    let jpeg = load_local_image(
        &fixture.image("jpeg.bin", &jpeg),
        ImageLimits::default(),
        &token,
    )
    .unwrap();
    let webp = load_local_image(
        &fixture.image("webp.bin", &webp),
        ImageLimits::default(),
        &token,
    )
    .unwrap();
    assert_eq!(
        (jpeg.width, jpeg.height, jpeg.rgba.len(), jpeg.rgba[3]),
        (1, 1, 4, 255)
    );
    assert_eq!(
        (webp.width, webp.height, webp.rgba),
        (1, 1, vec![0, 128, 255, 64])
    );
}

#[test]
fn visible_frame_ticket_drives_local_decode_and_resource_reflow() {
    let fixture = Fixture::new();
    fixture.image("pic.png", &png());
    let source = fixture.0.join("doc.md");
    fs::write(&source, "before\n\n![alt](pic.png)\n\nafter\n").unwrap();
    let token = Cancellation::default();
    let mut view = ReaderView::new(
        Limits::default(),
        ViewLimits::default(),
        Viewport::new(40, 20).unwrap(),
    )
    .unwrap();
    let load = view.open(&source).unwrap();
    assert_eq!(view.apply(load.execute()), ViewApply::Ready);
    let first = view.frame(&token).unwrap();
    let request = first
        .resources
        .iter()
        .find(|item| item.kind == loomterm_reader_core::view::ResourceKind::Image)
        .unwrap();
    let ticket = request.ticket.clone();
    let reference = match &first.projection.document.content.nodes[ticket.node].kind {
        NodeKind::Image { reference, .. } => reference.clone(),
        other => panic!("unexpected resource node: {other:?}"),
    };
    let decoded = load_local_image(&reference, ImageLimits::default(), &token).unwrap();
    assert_eq!((decoded.width, decoded.height), (2, 1));
    assert!(view
        .resource_size(
            &ticket,
            LogicalSize {
                columns: 2,
                rows: 1
            }
        )
        .unwrap());
    let updated = view.frame(&token).unwrap();
    assert!(!updated
        .resources
        .iter()
        .any(|item| item.ticket.node == ticket.node));
    view.close();
    drop(decoded);
}

#[test]
fn encoded_and_pixel_budgets_fail_without_installing_pixels() {
    let fixture = Fixture::new();
    let reference = fixture.image("pic.png", &png());
    let token = Cancellation::default();
    assert_eq!(
        load_local_image(
            &reference,
            ImageLimits {
                encoded_bytes: 1,
                ..ImageLimits::default()
            },
            &token
        )
        .unwrap_err(),
        ImageLoadError::EncodedBytes
    );
    assert_eq!(
        load_local_image(
            &reference,
            ImageLimits {
                pixels: 1,
                ..ImageLimits::default()
            },
            &token
        )
        .unwrap_err(),
        ImageLoadError::Dimensions
    );
    assert_eq!(
        load_local_image(
            &reference,
            ImageLimits {
                dimension: 1,
                ..ImageLimits::default()
            },
            &token
        )
        .unwrap_err(),
        ImageLoadError::Dimensions
    );
    assert_eq!(
        load_local_image(
            &reference,
            ImageLimits {
                pixels: 8_000_001,
                ..ImageLimits::default()
            },
            &token
        )
        .unwrap_err(),
        ImageLoadError::InvalidLimits
    );
}

#[test]
fn blocked_missing_corrupt_and_cancelled_images_are_explicit() {
    let fixture = Fixture::new();
    let token = Cancellation::default();
    let blocked = ImageReference::Blocked {
        destination: "https://invalid.example/pic.png".into(),
        reason: BlockedImage::NetworkOrScheme,
    };
    assert_eq!(
        load_local_image(&blocked, ImageLimits::default(), &token).unwrap_err(),
        ImageLoadError::Blocked(BlockedImage::NetworkOrScheme)
    );
    let missing = ImageReference::Local {
        path: fixture.0.join("missing.png"),
    };
    assert!(matches!(
        load_local_image(&missing, ImageLimits::default(), &token),
        Err(ImageLoadError::Source(ReaderError::Io {
            operation: "metadata",
            ..
        }))
    ));
    let directory = ImageReference::Local {
        path: fixture.0.clone(),
    };
    assert_eq!(
        load_local_image(&directory, ImageLimits::default(), &token).unwrap_err(),
        ImageLoadError::Source(ReaderError::NotRegularFile)
    );
    let corrupt = fixture.image("corrupt.png", b"not a PNG");
    assert_eq!(
        load_local_image(&corrupt, ImageLimits::default(), &token).unwrap_err(),
        ImageLoadError::UnsupportedFormat
    );
    let truncated = fixture.image("truncated.png", &png()[..8]);
    assert_eq!(
        load_local_image(&truncated, ImageLimits::default(), &token).unwrap_err(),
        ImageLoadError::Decode
    );
    token.cancel();
    assert_eq!(
        load_local_image(&corrupt, ImageLimits::default(), &token).unwrap_err(),
        ImageLoadError::Source(ReaderError::Cancelled)
    );
}
