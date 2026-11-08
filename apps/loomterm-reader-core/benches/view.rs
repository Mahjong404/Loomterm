use loomterm_reader_core::{view::*, Cancellation, Limits};
use std::{fs, path::PathBuf, time::Instant};

fn frame(view: &mut ReaderView, cancel: &Cancellation) -> Frame {
    for _ in 0..512 {
        let frame = view.frame(cancel).unwrap();
        if !frame.pending {
            return frame;
        }
    }
    panic!("frame work failed to converge");
}
fn main() {
    let cancel = Cancellation::default();
    let text = "Paragraph 世界 👩‍💻 **strong** and text.\n\n".repeat(10000);
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/view-bench-files")
        .join(std::process::id().to_string());
    fs::create_dir_all(&directory).unwrap();
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let fixture = Fixture(directory);
    let path = fixture.0.join("synthetic.md");
    fs::write(&path, &text).unwrap();
    let mut view = ReaderView::new(
        Limits::default(),
        ViewLimits::default(),
        Viewport::new(60, 30).unwrap(),
    )
    .unwrap();
    let start = Instant::now();
    let loaded = view.open(&path).unwrap().execute();
    assert_eq!(view.apply(loaded), ViewApply::Ready);
    let load_us = start.elapsed().as_micros();
    let start = Instant::now();
    let first = frame(&mut view, &cancel);
    let first_frame_us = start.elapsed().as_micros();
    let first_measured = first.budget.measured_blocks;
    drop(first);
    let mut scroll = Vec::new();
    let mut resize = Vec::new();
    for i in 0..200 {
        let start = Instant::now();
        view.input(
            Input::ScrollRows(if i % 20 == 0 {
                if i % 40 == 0 {
                    4000
                } else {
                    -4000
                }
            } else if i % 2 == 0 {
                3
            } else {
                -3
            }),
            &cancel,
        )
        .unwrap();
        let current = frame(&mut view, &cancel);
        std::hint::black_box(&current);
        scroll.push(start.elapsed().as_micros());
        if i % 10 == 0 {
            let start = Instant::now();
            view.resize(Viewport::new(if i % 20 == 0 { 30 } else { 60 }, 30).unwrap())
                .unwrap();
            let current = frame(&mut view, &cancel);
            std::hint::black_box(&current);
            resize.push(start.elapsed().as_micros());
        }
    }
    let start = Instant::now();
    let loaded = view.refresh().unwrap().unwrap().execute();
    assert_eq!(view.apply(loaded), ViewApply::Ready);
    let unchanged_us = start.elapsed().as_micros();
    fs::write(&path, format!("intro\n\n{text}")).unwrap();
    let start = Instant::now();
    let loaded = view.refresh().unwrap().unwrap().execute();
    assert_eq!(view.apply(loaded), ViewApply::Ready);
    let changed_us = start.elapsed().as_micros();
    let current = frame(&mut view, &cancel);
    scroll.sort();
    resize.sort();
    println!("{{\"source_bytes\":{},\"paragraphs\":10000,\"load_parse_project_us\":{},\"first_frame_us\":{},\"first_measured_blocks\":{},\"scroll_frames\":200,\"scroll_median_us\":{},\"scroll_p95_us\":{},\"resize_frames\":20,\"resize_median_us\":{},\"unchanged_reload_us\":{},\"changed_reload_us\":{},\"charged_cache_bytes_after_reload\":{},\"cache_blocks_after_reload\":{}}}",text.len(),load_us,first_frame_us,first_measured,scroll[100],scroll[190],resize[10],unchanged_us,changed_us,current.budget.cache_bytes,current.budget.cache_blocks);
}
