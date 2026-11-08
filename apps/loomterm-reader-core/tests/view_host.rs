use loomterm_reader_core::{view::*, Cancellation, Limits};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

struct MockHost {
    view: ReaderView,
    file: PathBuf,
}
impl MockHost {
    fn open(text: &str, columns: u16, rows: u16) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/view-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("document.md");
        fs::write(&file, text).unwrap();
        let mut view = ReaderView::new(
            Limits::default(),
            ViewLimits::default(),
            Viewport::new(columns, rows).unwrap(),
        )
        .unwrap();
        let loaded = view.open(&file).unwrap().execute();
        assert_eq!(view.apply(loaded), ViewApply::Ready);
        Self { view, file }
    }
    fn frame(&mut self) -> Frame {
        for _ in 0..512 {
            let frame = self.view.frame(&Cancellation::default()).unwrap();
            if !frame.pending {
                return frame;
            }
        }
        panic!("bounded frame work did not converge");
    }
}

#[test]
fn large_private_view_scrolls_lazily_resizes_and_reaches_both_ends() {
    let text = (0..10000)
        .map(|n| format!("Paragraph {n:05} 世界 text wraps when resized.\n\n"))
        .collect::<String>();
    let mut host = MockHost::open(&text, 60, 8);
    let projection = host.view.projection().unwrap().clone();
    let first = host.frame();
    assert!(first.budget.measured_blocks < 20);
    assert!(first.at_top && !first.extent_exact);
    host.view
        .input(Input::ScrollRows(8000), &Cancellation::default())
        .unwrap();
    let frame = host.frame();
    let anchor = frame.anchor.byte;
    assert!(anchor > 0 && frame.budget.measured_blocks < 40);
    for width in [20, 40, 12, 80, 30] {
        host.view.resize(Viewport::new(width, 8).unwrap()).unwrap();
        assert_eq!(host.frame().anchor.byte, anchor);
    }
    let builds = host.frame().budget.layout_builds;
    host.view
        .input(Input::ScrollRows(1), &Cancellation::default())
        .unwrap();
    assert!(host.frame().budget.layout_builds < builds + 4);
    assert!(Arc::ptr_eq(&projection, host.view.projection().unwrap()));
    host.view
        .input(Input::End, &Cancellation::default())
        .unwrap();
    assert!(host.frame().at_bottom);
    host.view
        .input(Input::Home, &Cancellation::default())
        .unwrap();
    assert!(host.frame().at_top);
}

#[test]
fn resource_dimensions_reflow_and_old_frames_and_foreign_callbacks_are_rejected() {
    let mut a = MockHost::open("before\n\n![alt](image.png)\n\nafter $x$\n", 30, 20);
    let mut b = MockHost::open("![alt](image.png)", 30, 20);
    let initial = a.frame();
    let image = initial
        .resources
        .iter()
        .find(|r| r.kind == ResourceKind::Image)
        .unwrap()
        .ticket
        .clone();
    let after_y = initial
        .lines
        .iter()
        .find(|l| l.clusters.iter().any(|c| initial.text(c).starts_with('a')))
        .unwrap()
        .y;
    assert!(a.view.is_current_frame(&initial));
    assert!(!b
        .view
        .resource_size(
            &image,
            LogicalSize {
                columns: 12,
                rows: 7
            }
        )
        .unwrap());
    assert!(a
        .view
        .resource_size(
            &image,
            LogicalSize {
                columns: 12,
                rows: 7
            }
        )
        .unwrap());
    assert!(!a.view.is_current_frame(&initial));
    let updated = a.frame();
    let new_y = updated
        .lines
        .iter()
        .find(|l| l.clusters.iter().any(|c| updated.text(c).starts_with('a')))
        .unwrap()
        .y;
    assert_eq!(new_y, after_y + 4);
    assert_eq!(
        a.view.resource_status(image.node),
        Some(ResourceStatus::Ready(LogicalSize {
            columns: 12,
            rows: 7
        }))
    );
    assert!(!updated
        .resources
        .iter()
        .any(|r| r.ticket.node == image.node));
    let math = updated
        .resources
        .iter()
        .find(|r| r.kind == ResourceKind::Math)
        .unwrap()
        .ticket
        .clone();
    a.view
        .resource_failed(&math, ResourceFailure::Unsupported)
        .unwrap();
    assert_eq!(
        a.view.resource_status(math.node),
        Some(ResourceStatus::Failed(ResourceFailure::Unsupported))
    );
    assert!(a.frame().resources.is_empty());
    a.view.resize(Viewport::new(15, 10).unwrap()).unwrap();
    assert!(!a.view.is_current_frame(&updated));
}

#[test]
fn host_can_navigate_search_hit_test_select_and_inspect_links_without_opening_them() {
    let mut host=MockHost::open("# First\n\n[inside](#second) [local](../note.md#topic) [web](https://example.invalid) [bad](javascript:alert)\n\n# Second\n\n中文 e\u{301} target\n",24,8);
    let projection = host.view.projection().unwrap().clone();
    assert_eq!(projection.headings.len(), 2);
    host.view
        .input(Input::Heading(1), &Cancellation::default())
        .unwrap();
    assert!(host
        .frame()
        .lines
        .iter()
        .any(|l| matches!(l.role, BlockRole::Heading(1))));
    let hits = projection
        .search("target", 10, &Cancellation::default())
        .unwrap();
    assert_eq!(hits.len(), 1);
    host.view
        .locate_search(&hits[0], &Cancellation::default())
        .unwrap();
    let frame = host.frame();
    let glyph = frame
        .lines
        .iter()
        .flat_map(|l| &l.clusters)
        .find(|c| frame.text(c) == "中")
        .unwrap();
    let row = frame
        .lines
        .iter()
        .find(|l| l.clusters.iter().any(|c| frame.text(c) == "中"))
        .unwrap()
        .y;
    let hit = frame.hit_test(glyph.x as u16 + 1, row as u16).unwrap();
    assert_eq!(frame.text(hit), "中");
    let text = &projection.document.content.source;
    let start = text.find("e\u{301}").unwrap();
    assert!(host
        .view
        .input(
            Input::Select {
                start,
                end: start + 1
            },
            &Cancellation::default()
        )
        .is_err());
    host.view
        .input(
            Input::Select {
                start,
                end: start + 3,
            },
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(host.view.selection(), Some("e\u{301}"));
    for expected in ["fragment", "local", "external", "blocked"] {
        host.view
            .input(
                Input::FocusLink { backwards: false },
                &Cancellation::default(),
            )
            .unwrap();
        let action = host
            .view
            .input(Input::ActivateLink, &Cancellation::default())
            .unwrap();
        match (expected, action) {
            ("fragment", Action::Link(LinkTarget::Fragment(f))) => assert_eq!(f, "second"),
            ("local", Action::Link(LinkTarget::LocalFile { path, fragment })) => {
                assert_eq!(path, host.file.parent().unwrap().join("../note.md"));
                assert_eq!(fragment.as_deref(), Some("topic"));
            }
            ("external", Action::Link(LinkTarget::External(s))) => {
                assert_eq!(s, "https://example.invalid")
            }
            ("blocked", Action::Link(LinkTarget::Blocked)) => {}
            value => panic!("unexpected navigation: {value:?}"),
        }
    }
    assert!(projection.search("", 10, &Cancellation::default()).is_err());
    assert!(host
        .view
        .input(Input::Heading(100), &Cancellation::default())
        .is_err());
}

#[test]
fn refresh_restores_source_anchor_and_cancelled_jobs_cannot_replace_current_view() {
    let source = "# Before\n\nintro\n\n# Target\n\nbody ![local](a.png)\n";
    let mut host = MockHost::open(source, 30, 8);
    host.view
        .input(Input::Heading(1), &Cancellation::default())
        .unwrap();
    let old = host.frame();
    let byte = old.anchor.byte;
    let ticket = old.resources.first().unwrap().ticket.clone();
    let old_hit = old
        .projection
        .search("body", 1, &Cancellation::default())
        .unwrap()
        .remove(0);
    let prefix = "new introduction\n\n";
    fs::write(&host.file, format!("{prefix}{source}")).unwrap();
    let loaded = host.view.refresh().unwrap().unwrap().execute();
    assert_eq!(host.view.apply(loaded), ViewApply::Ready);
    let new = host.frame();
    assert_eq!(new.anchor.byte, byte + prefix.len());
    assert_eq!(new.anchor.precision, Precision::Exact);
    assert!(!host
        .view
        .resource_size(
            &ticket,
            LogicalSize {
                columns: 5,
                rows: 2
            }
        )
        .unwrap());
    assert!(host
        .view
        .locate_search(&old_hit, &Cancellation::default())
        .is_err());
    let unchanged = host.view.projection().unwrap().clone();
    let loaded = host.view.refresh().unwrap().unwrap().execute();
    host.view.apply(loaded);
    assert!(Arc::ptr_eq(&unchanged, host.view.projection().unwrap()));
    let cancelled = host.view.refresh().unwrap().unwrap();
    cancelled.cancellation().cancel();
    assert_eq!(host.view.apply(cancelled.execute()), ViewApply::Cancelled);
    let old = host.view.refresh().unwrap().unwrap().execute();
    fs::write(&host.file, "# New target\n\nchanged").unwrap();
    let new = host.view.open(&host.file).unwrap().execute();
    assert_eq!(host.view.apply(new), ViewApply::Ready);
    assert_eq!(host.view.apply(old), ViewApply::Stale);
    assert!(host
        .view
        .projection()
        .unwrap()
        .document
        .content
        .source
        .contains("New target"));
    fs::write(&host.file, [0xff]).unwrap();
    let bad = host.view.refresh().unwrap().unwrap().execute();
    assert!(matches!(
        host.view.apply(bad),
        ViewApply::Failed(ViewError::Core(_))
    ));
    assert!(host.view.error().is_some());
    assert!(host
        .view
        .projection()
        .unwrap()
        .document
        .content
        .source
        .contains("New target"));
    let pending = host.view.refresh().unwrap().unwrap();
    let token = pending.cancellation();
    let weak = Arc::downgrade(host.view.projection().unwrap());
    host.view.close();
    assert!(token.is_cancelled());
    assert_eq!(host.view.apply(pending.execute()), ViewApply::Stale);
    // Worker and host-held old frames have explicit, ordinary Arc lifetimes.
    assert!(weak.upgrade().is_none());
    assert!(matches!(
        host.view.frame(&Cancellation::default()),
        Err(ViewError::NoDocument)
    ));
}

#[test]
fn semantics_long_code_and_wide_tables_keep_horizontal_overflow_explicit() {
    let code = (0..4000)
        .map(|n| format!("LINE-{n:04} {}END\n", "x".repeat(120)))
        .collect::<String>();
    let table = format!(
        "| left | right |\n|:--|--:|\n| {}TAIL | last |\n",
        "中".repeat(80)
    );
    let text=format!("# Styles\n\n> quote **strong** *emphasis* ~~strike~~ `inline`\n\n3. item\n4. next\n   - nested\n\n{table}\n```rust\n{code}```\n\n\u{1b}[31m inert <script>alert(1)</script>\n");
    let mut host = MockHost::open(&text, 20, 8);
    let projection = host.view.projection().unwrap().clone();
    assert!(projection.blocks.iter().any(|b| b.quote_depth == 1));
    assert!(projection.blocks.iter().any(|b| b.list_depth > 1));
    assert!(projection
        .blocks
        .iter()
        .any(|b| b.text.starts_with("3. item")));
    assert!(projection
        .blocks
        .iter()
        .flat_map(|b| &b.spans)
        .any(|s| s.style.strong));
    assert!(projection
        .blocks
        .iter()
        .flat_map(|b| &b.spans)
        .any(|s| s.style.code));
    let hit = projection
        .search("LINE-3999", 1, &Cancellation::default())
        .unwrap()
        .remove(0);
    host.view
        .locate_search(&hit, &Cancellation::default())
        .unwrap();
    let frame = host.frame();
    assert!(frame
        .lines
        .iter()
        .any(|l| l.role == BlockRole::Code && l.horizontal_overflow));
    host.view
        .input(Input::Horizontal(125), &Cancellation::default())
        .unwrap();
    let frame = host.frame();
    assert!(frame
        .lines
        .iter()
        .filter(|l| l.role == BlockRole::Code)
        .any(|l| l
            .clusters
            .iter()
            .map(|c| frame.text(c))
            .collect::<String>()
            .contains("END")));
    let hit = projection
        .search("TAIL", 1, &Cancellation::default())
        .unwrap()
        .remove(0);
    host.view
        .locate_search(&hit, &Cancellation::default())
        .unwrap();
    let frame = host.frame();
    assert!(frame
        .lines
        .iter()
        .any(|l| matches!(l.role, BlockRole::TableRow { .. }) && l.horizontal_overflow));
    assert!(!projection.blocks.iter().any(|b| b.text.contains('\u{1b}')));
    assert!(projection
        .blocks
        .iter()
        .any(|b| b.text.contains("<script>")));
}

#[test]
fn cache_frame_and_projection_budgets_fail_explicitly_and_views_stay_private() {
    let text = (0..200)
        .map(|n| format!("# H{n}\n\nprivate paragraph {n}\n\n"))
        .collect::<String>();
    let mut a = MockHost::open(&text, 20, 2);
    let mut b = MockHost::open(&text, 20, 2);
    let document = a.view.projection().unwrap().document.clone();
    for limits in [
        ViewLimits {
            blocks: 1,
            ..ViewLimits::default()
        },
        ViewLimits {
            runs: 1,
            ..ViewLimits::default()
        },
        ViewLimits {
            text_bytes: 2,
            ..ViewLimits::default()
        },
        ViewLimits {
            block_bytes: 2,
            ..ViewLimits::default()
        },
    ] {
        assert!(matches!(
            Projection::build(document.clone(), limits, &Cancellation::default()),
            Err(ViewError::Budget(_))
        ));
    }
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(matches!(
        a.view.frame(&cancelled),
        Err(ViewError::Core(
            loomterm_reader_core::ReaderError::Cancelled
        ))
    ));
    assert!(document.content.stats.nodes > 0);
    let original = b.frame().anchor.byte;
    for i in 1..100 {
        a.view
            .input(Input::Heading(i), &Cancellation::default())
            .unwrap();
        let frame = a.frame();
        assert!(frame.budget.cache_blocks <= 64 && frame.budget.cache_bytes <= 8 * 1024 * 1024);
    }
    assert_eq!(b.frame().anchor.byte, original);
    b.view
        .input(
            Input::FocusLink { backwards: true },
            &Cancellation::default(),
        )
        .unwrap();
    assert!(b.view.focused_link().is_none());
    let weak = Arc::downgrade(a.view.projection().unwrap());
    a.view.close();
    assert!(weak.upgrade().is_none());
    assert!(b.view.projection().is_some());
    assert!(Viewport::new(0, 10).is_err());
    assert!(b
        .view
        .resize(Viewport {
            columns: 0,
            rows: 1
        })
        .is_err());
    assert!(b
        .view
        .set_metrics(Metrics {
            tab_stop: 0,
            ..Metrics::default()
        })
        .is_err());

    let mut constrained = ReaderView::new(
        Limits::default(),
        ViewLimits {
            frame_clusters: 1,
            ..ViewLimits::default()
        },
        Viewport::new(20, 8).unwrap(),
    )
    .unwrap();
    let loaded = constrained.open(&b.file).unwrap().execute();
    constrained.apply(loaded);
    assert!(matches!(
        constrained.frame(&Cancellation::default()),
        Err(ViewError::Budget(ViewResource::FrameClusters))
    ));
    assert!(constrained.error().is_some());
    let mut tiny = ReaderView::new(
        Limits::default(),
        ViewLimits {
            cache_bytes: 0,
            cache_blocks: 0,
            work_blocks: 1,
            ..ViewLimits::default()
        },
        Viewport::new(20, 8).unwrap(),
    )
    .unwrap();
    let loaded = tiny.open(&b.file).unwrap().execute();
    tiny.apply(loaded);
    let _ = tiny.frame(&Cancellation::default());
    assert!(matches!(
        tiny.frame(&Cancellation::default()),
        Err(ViewError::Budget(ViewResource::CacheBytes))
    ));
    for limits in [
        Limits {
            source_bytes: 2,
            ..Limits::default()
        },
        Limits {
            nodes: 1,
            ..Limits::default()
        },
    ] {
        let mut view =
            ReaderView::new(limits, ViewLimits::default(), Viewport::new(20, 8).unwrap()).unwrap();
        let loaded = view.open(&b.file).unwrap().execute();
        assert!(matches!(
            view.apply(loaded),
            ViewApply::Failed(ViewError::Core(loomterm_reader_core::ReaderError::Limit(_)))
        ));
        assert!(view.projection().is_none());
    }
}
impl Drop for MockHost {
    fn drop(&mut self) {
        self.view.close();
        fs::remove_dir_all(self.file.parent().unwrap()).unwrap();
    }
}

#[derive(Default)]
struct MockRenderer {
    drawn: Vec<String>,
}
impl MockRenderer {
    fn paint(&mut self, view: &ReaderView, frame: &Frame) -> bool {
        if !view.is_current_frame(frame) {
            return false;
        }
        self.drawn.clear();
        for line in &frame.lines {
            assert!(line.y < i32::from(frame.viewport.rows) && line.y + i32::from(line.height) > 0);
            for glyph in &line.clusters {
                assert!(
                    glyph.x < i32::from(frame.viewport.columns) && glyph.x + glyph.width as i32 > 0
                );
                self.drawn.push(frame.text(glyph).replace('\t', " "));
            }
        }
        true
    }
}

#[test]
fn mock_adapter_coalesces_targets_and_only_paints_current_revision() {
    let mut host = MockHost::open("# Initial\n\ntext", 20, 8);
    let old_frame = host.frame();
    let mut renderer = MockRenderer::default();
    assert!(renderer.paint(&host.view, &old_frame));
    let active = host.view.refresh().unwrap().unwrap();
    let mut pending = None;
    for n in 0..100 {
        fs::write(&host.file, format!("# Revision {n}\n\nlatest")).unwrap();
        // One active worker and one replaceable pending job, no growing queue.
        pending = Some(host.view.open(&host.file).unwrap());
    }
    assert_eq!(host.view.apply(active.execute()), ViewApply::Stale);
    assert_eq!(
        host.view.apply(pending.take().unwrap().execute()),
        ViewApply::Ready
    );
    assert!(!renderer.paint(&host.view, &old_frame));
    let frame = host.frame();
    assert!(renderer.paint(&host.view, &frame));
    assert!(renderer.drawn.concat().contains("Revision 99"));
    host.view.resize(Viewport::new(10, 6).unwrap()).unwrap();
    assert!(!renderer.paint(&host.view, &frame));
    let frame = host.frame();
    assert!(renderer.paint(&host.view, &frame));
    host.view.close();
    assert!(!renderer.paint(&host.view, &frame));
}

#[test]
fn deletion_of_reading_anchor_is_reported_as_approximate() {
    let mut host = MockHost::open("# Original\n\ntext\n", 20, 8);
    assert_eq!(host.frame().anchor.precision, Precision::Exact);
    fs::write(&host.file, "# Replacement\n\nother\n").unwrap();
    let loaded = host.view.refresh().unwrap().unwrap().execute();
    host.view.apply(loaded);
    assert_eq!(host.frame().anchor.precision, Precision::Approximate);
    host.view
        .input(Input::Home, &Cancellation::default())
        .unwrap();
    assert_eq!(host.frame().anchor.precision, Precision::Exact);
}

#[test]
fn display_math_remains_a_standalone_resource_and_reflows_without_rendering_tex() {
    let mut host = MockHost::open("$$\nx^2 + y^2\n$$\n\nafter\n", 30, 20);
    let initial = host.frame();
    assert_eq!(initial.resources.len(), 1);
    let request = &initial.resources[0];
    assert_eq!(request.kind, ResourceKind::Math);
    assert!(matches!(
        &initial.projection.document.content.nodes[request.ticket.node].kind,
        loomterm_reader_core::NodeKind::Math { display: true, source } if source.contains("x^2 + y^2")
    ));
    let line = initial
        .lines
        .iter()
        .find(|line| {
            line.clusters
                .iter()
                .any(|c| c.resource == Some(request.ticket.node))
        })
        .unwrap();
    assert_eq!(line.height, 2);
    host.view
        .resource_size(
            &request.ticket,
            LogicalSize {
                columns: 12,
                rows: 4,
            },
        )
        .unwrap();
    let updated = host.frame();
    assert!(!host.view.is_current_frame(&initial));
    assert_eq!(updated.lines[0].height, 4);
    assert!(updated.resources.is_empty());
}

#[test]
fn host_gets_semantic_grapheme_layout_without_a_terminal_renderer() {
    let mut host = MockHost::open("# 标题\n\nAe\u{301}中👩‍💻B\n", 4, 12);
    let frame = host.frame();
    let texts: Vec<String> = frame
        .lines
        .iter()
        .map(|line| line.clusters.iter().map(|c| frame.text(c)).collect())
        .collect();
    assert!(
        texts.iter().any(|line| line.contains("Ae\u{301}中")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|line| line.contains("👩‍💻B")), "{texts:?}");
    assert!(frame
        .lines
        .iter()
        .any(|line| matches!(line.role, BlockRole::Heading(1))));
    assert!(frame
        .lines
        .iter()
        .flat_map(|line| &line.clusters)
        .all(|c| c.width <= 2));
}
