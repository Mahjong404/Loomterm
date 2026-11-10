use loomterm_reader_core::{math::*, Cancellation};
use std::time::Instant;

fn main() {
    let source = r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}";
    let token = Cancellation::default();
    let start = Instant::now();
    let first = render_math(
        source,
        true,
        MathOptions::default(),
        MathLimits::default(),
        &token,
    )
    .unwrap();
    let cold = start.elapsed().as_micros();
    let mut times = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let result = render_math(
            source,
            true,
            MathOptions::default(),
            MathLimits::default(),
            &token,
        )
        .unwrap();
        times.push(start.elapsed().as_micros());
        assert_eq!(result.image.rgba, first.image.rgba);
    }
    times.sort_unstable();
    println!(
        "{{\"formula\":\"quadratic\",\"samples\":20,\"cold_us\":{cold},\"median_us\":{},\"p95_us\":{},\"width\":{},\"height\":{},\"rgba_bytes\":{}}}",
        times[10], times[18], first.image.width, first.image.height, first.image.rgba.len()
    );
}
