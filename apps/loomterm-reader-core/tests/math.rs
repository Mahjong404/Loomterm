#![cfg(feature = "math")]
use loomterm_reader_core::{math::*, Cancellation};

#[test]
fn offline_inline_display_and_common_math_produce_real_rgba() {
    for source in [
        r"E=mc^2",
        r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
        r"\sum_{i=1}^{n}i=\frac{n(n+1)}{2}",
        r"\int_0^\infty e^{-x}\,dx=1",
        r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
        r"\begin{aligned}a&=b+c\\d&=\sqrt{x}\end{aligned}",
        r"\alpha+\beta=\theta",
        r"\text{hello world}",
    ] {
        for display in [false, true] {
            let raster = render_math(
                source,
                display,
                MathOptions::default(),
                MathLimits::default(),
                &Cancellation::default(),
            )
            .unwrap_or_else(|error| panic!("{source} / {display}: {error:?}"));
            assert_eq!(
                raster.image.rgba.len(),
                raster.image.width as usize * raster.image.height as usize * 4
            );
            assert!(raster.baseline_px > 0. && raster.baseline_px <= raster.image.height as f32);
            assert!(raster.image.rgba.chunks_exact(4).any(|p| p[3] > 0));
            assert!(raster.image.rgba.chunks_exact(4).any(|p| p[3] == 0));
        }
    }
}

#[test]
fn formula_limits_cancellation_and_visible_error_contract() {
    let render = |source: &str, limits| {
        render_math(
            source,
            false,
            MathOptions::default(),
            limits,
            &Cancellation::default(),
        )
    };
    assert_eq!(
        render("", MathLimits::default()).unwrap_err(),
        MathError::InvalidFormula
    );
    for source in [
        r"\input{secret}",
        r"\def\a{\a}\a",
        r"\href{https://x}{x}",
        r"\includegraphics{x}",
        "中文",
        r"\unknown",
    ] {
        assert_eq!(
            render(source, MathLimits::default()).unwrap_err(),
            MathError::Unsupported
        );
    }
    assert_eq!(
        render(r"\frac{", MathLimits::default()).unwrap_err(),
        MathError::InvalidFormula
    );
    assert_eq!(
        render(
            "abc",
            MathLimits {
                source_bytes: 2,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::SourceBudget
    );
    assert_eq!(
        render(
            "abc",
            MathLimits {
                tokens: 2,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::Complexity
    );
    assert_eq!(
        render(
            "{{x}}",
            MathLimits {
                depth: 1,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::Complexity
    );
    assert_eq!(
        render(
            "x+y",
            MathLimits {
                draw_items: 1,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::OutputBudget
    );
    assert_eq!(
        render(
            "x",
            MathLimits {
                pixels: 1,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::OutputBudget
    );
    assert_eq!(
        render(
            "x",
            MathLimits {
                dimension: 1,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::OutputBudget
    );
    assert_eq!(
        render(
            "x",
            MathLimits {
                depth: 33,
                ..MathLimits::default()
            }
        )
        .unwrap_err(),
        MathError::InvalidLimits
    );
    let cancel = Cancellation::default();
    cancel.cancel();
    assert_eq!(
        render_math(
            "x",
            true,
            MathOptions::default(),
            MathLimits::default(),
            &cancel
        )
        .unwrap_err(),
        MathError::Cancelled
    );
}

#[test]
fn dpi_changes_dimensions_and_returned_buffers_are_independent() {
    let first = render_math(
        r"\frac{a}{b}",
        true,
        MathOptions::default(),
        MathLimits::default(),
        &Cancellation::default(),
    )
    .unwrap();
    let second = render_math(
        r"\frac{a}{b}",
        true,
        MathOptions {
            font_px: 48,
            ..MathOptions::default()
        },
        MathLimits::default(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(second.image.width > first.image.width);
    assert!(second.image.height > first.image.height);
    assert_ne!(first.image.rgba.as_ptr(), second.image.rgba.as_ptr());
}

#[test]
fn formula_raster_reflows_only_its_current_private_resource_ticket() {
    use loomterm_reader_core::{view::*, Limits, NodeKind};
    use std::{fs, path::PathBuf};
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let fixture = Fixture(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/math-fixtures")
            .join(std::process::id().to_string()),
    );
    fs::create_dir_all(&fixture.0).unwrap();
    let path = fixture.0.join("document.md");
    fs::write(&path, "before\n\n$$\\frac{a}{b}$$\n\nafter\n").unwrap();
    let cancel = Cancellation::default();
    let mut view = ReaderView::new(
        Limits::default(),
        ViewLimits::default(),
        Viewport::new(40, 20).unwrap(),
    )
    .unwrap();
    let request = view.open(&path).unwrap();
    assert_eq!(view.apply(request.execute()), ViewApply::Ready);
    let first = view.frame(&cancel).unwrap();
    let ticket = &first
        .resources
        .iter()
        .find(|r| r.kind == ResourceKind::Math)
        .unwrap()
        .ticket;
    let (display, source) = match &first.projection.document.content.nodes[ticket.node].kind {
        NodeKind::Math { display, source } => (*display, source.as_str()),
        _ => panic!("math ticket must retain its math syntax"),
    };
    let raster = render_math(
        source,
        display,
        MathOptions::default(),
        MathLimits::default(),
        &cancel,
    )
    .unwrap();
    let size = LogicalSize {
        columns: raster.image.width.div_ceil(8) as u16,
        rows: raster.image.height.div_ceil(18) as u16,
    };
    assert!(view.resource_size(ticket, size).unwrap());
    assert!(!view.is_current_frame(&first));
    for columns in [20, 48, 12, 40] {
        let before_resize = view.frame(&cancel).unwrap();
        view.resize(Viewport::new(columns, 20).unwrap()).unwrap();
        assert!(!view.is_current_frame(&before_resize));
        let after_resize = view.frame(&cancel).unwrap();
        assert!(after_resize.resources.is_empty());
        assert_eq!(
            view.resource_status(ticket.node),
            Some(ResourceStatus::Ready(size))
        );
        assert!(after_resize.lines.iter().any(|line| {
            line.clusters
                .iter()
                .any(|cluster| cluster.resource == Some(ticket.node))
        }));
    }
    fs::write(&path, "$$x^2$$\n").unwrap();
    let refresh = view.refresh().unwrap().unwrap();
    assert_eq!(view.apply(refresh.execute()), ViewApply::Ready);
    assert!(!view.resource_size(ticket, size).unwrap());
    view.close();
    assert_eq!(view.resource_size(ticket, size), Err(ViewError::NoDocument));
}

#[test]
fn invalid_display_math_keeps_source_and_exposes_failure_to_host() {
    use loomterm_reader_core::{view::*, Limits, NodeKind};
    use std::{fs, path::PathBuf};
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let fixture = Fixture(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/math-fixtures")
            .join(format!("invalid-{}", std::process::id())),
    );
    fs::create_dir_all(&fixture.0).unwrap();
    let source = "$$\\unknown$$\n";
    let path = fixture.0.join("invalid.md");
    fs::write(&path, source).unwrap();
    let mut view = ReaderView::new(
        Limits::default(),
        ViewLimits::default(),
        Viewport::new(30, 10).unwrap(),
    )
    .unwrap();
    let request = view.open(&path).unwrap();
    assert_eq!(view.apply(request.execute()), ViewApply::Ready);
    let cancel = Cancellation::default();
    let frame = view.frame(&cancel).unwrap();
    let ticket = frame
        .resources
        .iter()
        .find(|r| r.kind == ResourceKind::Math)
        .unwrap()
        .ticket
        .clone();
    let formula = match &frame.projection.document.content.nodes[ticket.node].kind {
        NodeKind::Math { source, .. } => source,
        other => panic!("unexpected node: {other:?}"),
    };
    assert_eq!(
        render_math(
            formula,
            true,
            MathOptions::default(),
            MathLimits::default(),
            &cancel,
        )
        .unwrap_err(),
        MathError::Unsupported
    );
    assert!(view
        .resource_failed(&ticket, ResourceFailure::Unsupported)
        .unwrap());
    assert_eq!(
        view.resource_status(ticket.node),
        Some(ResourceStatus::Failed(ResourceFailure::Unsupported))
    );
    assert!(view.frame(&cancel).unwrap().resources.is_empty());
    assert_eq!(frame.projection.document.content.source, source);
    view.close();
}
