use loomterm_reader_core::{parse_markdown, Apply, Cancellation, Limits, Reader, Update};
use std::{hint::black_box, time::Instant};

fn main() {
    let input =
        "## Unicode 世界 👩‍💻\n\nText **bold** and $a^2+b^2$.\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n"
            .repeat(4096);
    let token = Cancellation::default();
    let limits = Limits::default();
    let mut times = Vec::new();
    let mut retained = 0;
    let mut nodes = 0;
    for _ in 0..5 {
        let start = Instant::now();
        let content = parse_markdown(input.clone(), None, limits, &token).unwrap();
        times.push(start.elapsed().as_micros());
        retained = content.retained_bytes();
        nodes = content.nodes.len();
        black_box(&content);
    }
    times.sort();
    let plain = "a".repeat(4 * 1024 * 1024);
    let start = Instant::now();
    let content = parse_markdown(plain, None, Limits::default(), &token).unwrap();
    let plain_us = start.elapsed().as_micros();
    let plain_retained = content.retained_bytes();
    drop(content);

    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/bench-files")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let cleanup = Cleanup(directory);
    let path = cleanup.0.join("document.md");
    std::fs::write(&path, &input).unwrap();
    let mut reader = Reader::new(limits).unwrap();
    let loaded = reader.open(&path).unwrap().execute();
    assert!(matches!(reader.apply(loaded), Apply::Ready(_)));
    let start = Instant::now();
    let loaded = reader.reload().unwrap().unwrap().execute();
    assert_eq!(reader.apply(loaded), Apply::Ready(Update::Unchanged));
    let unchanged_us = start.elapsed().as_micros();
    std::fs::write(&path, format!("{input}\nupdated")).unwrap();
    let start = Instant::now();
    let loaded = reader.reload().unwrap().unwrap().execute();
    assert!(matches!(
        reader.apply(loaded),
        Apply::Ready(Update::Replaced { .. })
    ));
    let changed_us = start.elapsed().as_micros();
    let pending = reader.reload().unwrap().unwrap();
    pending.cancellation().cancel();
    let start = Instant::now();
    assert_eq!(reader.apply(pending.execute()), Apply::Cancelled);
    let cancelled_us = start.elapsed().as_micros();
    drop(reader);
    drop(cleanup);
    println!("{{\"source_bytes\":{},\"nodes\":{},\"retained_bytes_estimate\":{},\"parse_median_us\":{},\"runs\":5,\"plain_4mib_us\":{},\"plain_4mib_retained_bytes_estimate\":{},\"unchanged_reload_us\":{},\"changed_reload_us\":{},\"precancelled_request_us\":{}}}", input.len(), nodes, retained, times[2], plain_us, plain_retained, unchanged_us, changed_us, cancelled_us);
}
