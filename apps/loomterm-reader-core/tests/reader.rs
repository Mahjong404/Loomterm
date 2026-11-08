use loomterm_reader_core::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

// Each fixture owns one directory under this crate's ignored target, removed on Drop.
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-files")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("fixture files must be closed and removable");
    }
}
fn parse(source: &str) -> Content {
    parse_markdown(
        source.into(),
        None,
        Limits::default(),
        &Cancellation::default(),
    )
    .unwrap()
}
fn open(reader: &mut Reader, path: &Path) -> Apply {
    let result = reader.open(path).unwrap().execute();
    reader.apply(result)
}

#[test]
fn commonmark_blocks_inlines_tables_math_and_raw_html() {
    let source = "# Heading\n\nParagraph **bold** *em* ~~strike~~ `code` [link][ref].\n\n> quote\n\n3. ordered\n4. next\n\n- [x] done\n- nested\n  - child\n\n| A | B |\n|:--|--:|\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n    indented\n\nInline $a^2$ and $$b^2$$.\n\n![alt *em*](pic.png)\n\n---\n\n<script>alert(1)</script>\n\n[ref]: https://example.invalid \"title\"\n";
    let content = parse(source);
    for predicate in [
        (|k: &NodeKind| matches!(k, NodeKind::Heading(1))) as fn(&NodeKind) -> bool,
        |k| matches!(k, NodeKind::Paragraph),
        |k| matches!(k, NodeKind::BlockQuote),
        |k| matches!(k, NodeKind::List { start: Some(3) }),
        |k| matches!(k, NodeKind::List { start: None }),
        |k| matches!(k, NodeKind::Table { alignments } if alignments == &[Alignment::Left, Alignment::Right]),
        |k| matches!(k, NodeKind::TableHead),
        |k| matches!(k, NodeKind::TableRow),
        |k| matches!(k, NodeKind::TableCell),
        |k| matches!(k, NodeKind::CodeBlock { info } if info == "rust"),
        |k| matches!(k, NodeKind::CodeBlock { info } if info.is_empty()),
        |k| matches!(k, NodeKind::Emphasis),
        |k| matches!(k, NodeKind::Strong),
        |k| matches!(k, NodeKind::Strikethrough),
        |k| matches!(k, NodeKind::Code(s) if s == "code"),
        |k| matches!(k, NodeKind::Link { destination, title } if destination == "https://example.invalid" && title == "title"),
        |k| matches!(k, NodeKind::Image { .. }),
        |k| matches!(k, NodeKind::Math { display: false, source } if source == "a^2"),
        |k| matches!(k, NodeKind::Math { display: true, source } if source == "b^2"),
        |k| matches!(k, NodeKind::TaskMarker(true)),
        |k| matches!(k, NodeKind::RawHtml(s) if s.contains("<script>")),
        |k| matches!(k, NodeKind::Rule),
    ] {
        assert!(
            content.nodes.iter().any(|node| predicate(&node.kind)),
            "missing expected node: {:?}",
            content.nodes
        );
    }
    assert_eq!(content.source, source);
    for (id, node) in content.nodes.iter().enumerate() {
        assert!(node.span.end <= source.len());
        assert!(source.is_char_boundary(node.span.start) && source.is_char_boundary(node.span.end));
        assert!(node.subtree_end > id && node.subtree_end <= content.nodes.len());
        if let Some(parent) = node.parent {
            assert!(parent < id && id < content.nodes[parent].subtree_end);
        }
    }
    let top: Vec<_> = content.children(0).collect();
    assert!(top.len() > 8 && top.iter().all(|(_, node)| node.parent == Some(0)));
}

#[test]
fn unicode_bom_entities_breaks_and_code_are_preserved() {
    let source = "\u{feff}# 中文 👩‍💻 e\u{301} العربية\r\n\r\nA &amp; B  \r\nnext\r\nsoft\r\n\r\n`$not_math$`\r\n\r\n```\r\n$$not_math$$\r\n```\r\n";
    let content = parse(source);
    assert_eq!(content.source, source);
    assert!(content
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Heading(1))));
    assert!(content
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::HardBreak)));
    assert!(content
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::SoftBreak)));
    assert!(!content
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Math { .. })));
    for node in &content.nodes {
        assert!(content.source.get(node.span.clone()).is_some());
    }
    assert!(content
        .nodes
        .iter()
        .filter_map(|n| if let NodeKind::Text(s) = &n.kind {
            Some(s.as_str())
        } else {
            None
        })
        .any(|s| s.contains('&')));
}

#[test]
fn each_budget_is_enforced_and_partial_content_never_escapes() {
    for (source, limits, resource) in [
        (
            "1234",
            Limits {
                source_bytes: 3,
                ..Limits::default()
            },
            Resource::SourceBytes,
        ),
        (
            "# a",
            Limits {
                nodes: 2,
                ..Limits::default()
            },
            Resource::Nodes,
        ),
        (
            "> > > deep",
            Limits {
                depth: 2,
                ..Limits::default()
            },
            Resource::Depth,
        ),
        (
            "![a](a) ![b](b)",
            Limits {
                image_references: 1,
                ..Limits::default()
            },
            Resource::ImageReferences,
        ),
        (
            "abcdef",
            Limits {
                payload_bytes: 3,
                ..Limits::default()
            },
            Resource::PayloadBytes,
        ),
    ] {
        assert_eq!(
            parse_markdown(source.into(), None, limits, &Cancellation::default()).unwrap_err(),
            ReaderError::Limit(resource)
        );
    }
    assert!(Reader::new(Limits {
        depth: 129,
        ..Limits::default()
    })
    .is_err());
    assert_eq!(
        parse_markdown(
            String::new(),
            None,
            Limits {
                nodes: 0,
                ..Limits::default()
            },
            &Cancellation::default()
        )
        .unwrap_err(),
        ReaderError::Limit(Resource::Nodes)
    );
    assert_eq!(parse("").nodes.len(), 1);
    let input = "x".repeat(4 * 1024 * 1024 + 1);
    assert_eq!(
        parse_markdown(input, None, Limits::default(), &Cancellation::default()).unwrap_err(),
        ReaderError::Limit(Resource::SourceBytes)
    );
}

#[test]
fn local_images_resolve_against_document_without_access_or_containment_acl() {
    let fixture = Fixture::new();
    let document = fixture.0.join("docs/readme.md");
    let source = "![space](<images/a%20b.png>) ![parent](../outside.png#part) ![remote](https://example.invalid/x) ![unc](//server/share/a) ![data](data:image/png;base64,abc) ![file](file:///tmp/a) ![encoded](%2F%2Fserver/a) ![nul](a%00.png) ![device](NUL.png)";
    let content = parse_markdown(
        source.into(),
        Some(&document),
        Limits::default(),
        &Cancellation::default(),
    )
    .unwrap();
    let images: Vec<_> = content
        .nodes
        .iter()
        .filter_map(|n| {
            if let NodeKind::Image { reference, .. } = &n.kind {
                Some(reference)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(images.len(), 9);
    assert_eq!(
        *images[0],
        ImageReference::Local {
            path: fixture.0.join("docs/images/a b.png")
        }
    );
    assert_eq!(
        *images[1],
        ImageReference::Local {
            path: fixture.0.join("docs/../outside.png")
        }
    );
    assert!(images[2..]
        .iter()
        .all(|i| matches!(i, ImageReference::Blocked { .. })));
    assert!(!fixture.0.join("docs").exists()); // Parsing never probes/creates images.
    assert!(matches!(
        &parse("![x](a.png)").nodes[2].kind,
        NodeKind::Image {
            reference: ImageReference::Blocked {
                reason: BlockedImage::MissingDocumentContext,
                ..
            },
            ..
        }
    ));
}

#[test]
fn image_paths_cover_absolute_unicode_bad_escapes_and_foreign_namespaces() {
    let fixture = Fixture::new();
    let absolute = fixture.0.join("图片.png");
    let source = format!("![abs](<{}>) ![uni](图片.png) ![bad](bad%ZZ) ![scheme](javascript:alert) ![drive](C:relative) ![device](%5C%5C%3F%5CC:/a) ![ads](x.png:stream) ![query](a.png?size=2)", absolute.to_string_lossy().replace('\\', "/"));
    let content = parse_markdown(
        source,
        Some(&fixture.0.join("document.md")),
        Limits::default(),
        &Cancellation::default(),
    )
    .unwrap();
    let images: Vec<_> = content
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Image { reference, .. } => Some(reference),
            _ => None,
        })
        .collect();
    assert_eq!(images.len(), 8);
    match images[0] {
        ImageReference::Local { path } => assert_eq!(path, &absolute),
        _ => panic!("local absolute image blocked"),
    }
    assert_eq!(*images[1], ImageReference::Local { path: absolute });
    assert!(images[2..]
        .iter()
        .all(|i| matches!(i, ImageReference::Blocked { .. })));
    assert_eq!(
        parse_markdown(
            "![x](x)".into(),
            Some(Path::new("//server/share/doc.md")),
            Limits::default(),
            &Cancellation::default()
        )
        .unwrap_err(),
        ReaderError::NonLocalPath
    );
    let huge = PathBuf::from("a".repeat(MAX_PATH_BYTES + 1));
    assert_eq!(
        read_source(&huge, Limits::default(), &Cancellation::default()).unwrap_err(),
        ReaderError::Limit(Resource::PathBytes)
    );
}

#[test]
fn invalid_encoding_missing_directory_and_oversize_files() {
    let fixture = Fixture::new();
    let token = Cancellation::default();
    let invalid = fixture.file("invalid.md", [0x61, 0xff]);
    assert_eq!(
        read_source(&invalid, Limits::default(), &token).unwrap_err(),
        ReaderError::InvalidEncoding { valid_up_to: 1 }
    );
    let utf16 = fixture.file("utf16.md", [0xff, 0xfe, 0x61, 0]);
    assert!(matches!(
        read_source(&utf16, Limits::default(), &token),
        Err(ReaderError::InvalidEncoding { .. })
    ));
    assert!(matches!(
        read_source(&fixture.0.join("missing"), Limits::default(), &token),
        Err(ReaderError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        })
    ));
    assert_eq!(
        read_source(&fixture.0, Limits::default(), &token).unwrap_err(),
        ReaderError::NotRegularFile
    );
    let large = fixture.file("large.md", "0123456789");
    assert_eq!(
        read_source(
            &large,
            Limits {
                source_bytes: 3,
                ..Limits::default()
            },
            &token
        )
        .unwrap_err(),
        ReaderError::Limit(Resource::SourceBytes)
    );
    for bad in [
        "https://example.invalid/a",
        "//server/share/a",
        "\\\\.\\pipe\\x",
        "NUL",
        "a:stream",
    ] {
        assert_eq!(
            read_source(Path::new(bad), Limits::default(), &token).unwrap_err(),
            ReaderError::NonLocalPath
        );
    }
}

#[test]
fn refresh_pause_same_length_change_atomic_replace_delete_and_recreate() {
    let fixture = Fixture::new();
    let path = fixture.file("doc.md", "# first\n");
    let mut reader = Reader::new(Limits::default()).unwrap();
    assert!(reader.following());
    assert!(matches!(
        open(&mut reader, &path),
        Apply::Ready(Update::Replaced { .. })
    ));
    let first = reader.document().unwrap().content.clone();
    let refresh = reader.poll().unwrap().unwrap();
    assert!(reader.poll().unwrap().is_none()); // coalesce while busy
    assert_eq!(
        reader.apply(refresh.execute()),
        Apply::Ready(Update::Unchanged)
    );
    assert!(Arc::ptr_eq(&first, &reader.document().unwrap().content));
    reader.set_following(false);
    assert!(reader.poll().unwrap().is_none());
    let original_stamp = reader.document().unwrap().stamp.clone();
    fs::write(&path, "# other\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_stamp.modified.unwrap()))
        .unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.len(), original_stamp.bytes);
    assert_eq!(metadata.modified().ok(), original_stamp.modified); // identical advisory stamp
    let refresh = reader.reload().unwrap().unwrap();
    assert!(matches!(
        reader.apply(refresh.execute()),
        Apply::Ready(Update::Replaced { .. })
    ));
    fs::remove_file(&path).unwrap();
    let refresh = reader.reload().unwrap().unwrap();
    assert!(matches!(
        reader.apply(refresh.execute()),
        Apply::Failed(ReaderError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        })
    ));
    assert_eq!(reader.document().unwrap().content.source, "# other\n");
    let replacement = fixture.file("replacement.md", "新文档");
    fs::rename(&replacement, &path).unwrap();
    assert!(matches!(open(&mut reader, &path), Apply::Ready(_)));
    assert_eq!(reader.document().unwrap().content.source, "新文档");
    // Atomic replacement while the target exists (Windows ReplaceFile is not needed: rename overwrites a file).
    let replacement = fixture.file("replacement2.md", "原子保存");
    fs::rename(&replacement, &path).unwrap();
    assert!(matches!(open(&mut reader, &path), Apply::Ready(_)));
    assert_eq!(reader.document().unwrap().content.source, "原子保存");
}

#[test]
fn stale_cancelled_and_closed_results_cannot_replace_target_and_arcs_release() {
    let fixture = Fixture::new();
    let a = fixture.file("a.md", "a");
    let b = fixture.file("b.md", "b");
    let mut reader = Reader::new(Limits::default()).unwrap();
    let old = reader.open(&a).unwrap().execute();
    let newer = reader.open(&b).unwrap().execute();
    assert_eq!(reader.apply(old), Apply::Stale);
    assert!(matches!(reader.apply(newer), Apply::Ready(_)));
    let weak = Arc::downgrade(reader.document().unwrap());
    let request = reader.reload().unwrap().unwrap();
    let cancel = request.cancellation();
    cancel.cancel();
    assert_eq!(reader.apply(request.execute()), Apply::Cancelled);
    assert!(matches!(reader.state(), ReaderState::Cancelled { .. }));
    let late = reader.reload().unwrap().unwrap().execute();
    reader.cancel();
    assert_eq!(reader.apply(late), Apply::Stale);
    reader.clear();
    assert!(weak.upgrade().is_none());
    assert_eq!(reader.state(), &ReaderState::Empty);
    let request = reader.open(&a).unwrap();
    let token = request.cancellation();
    drop(reader);
    assert!(token.is_cancelled());
    drop(request.execute());
    // Source File handles must have closed before rename/delete (also tested by fixture Drop).
    fs::remove_file(a).unwrap();
}

#[test]
fn switch_failure_clears_old_target_and_clients_are_independent() {
    let fixture = Fixture::new();
    let path = fixture.file("doc.md", "first");
    let mut a = Reader::new(Limits::default()).unwrap();
    let mut b = Reader::new(Limits::default()).unwrap();
    open(&mut a, &path);
    open(&mut b, &path);
    a.set_following(false);
    fs::write(&path, "later").unwrap();
    let request = b.poll().unwrap().unwrap();
    b.apply(request.execute());
    assert_eq!(a.document().unwrap().content.source, "first");
    assert_eq!(b.document().unwrap().content.source, "later");
    assert!(matches!(
        open(&mut a, &fixture.0.join("missing")),
        Apply::Failed(_)
    ));
    assert!(a.document().is_none());
    assert_eq!(b.document().unwrap().content.source, "later");
}

#[test]
fn result_misrouted_from_another_client_is_rejected_even_with_same_path_and_generation() {
    let fixture = Fixture::new();
    let path = fixture.file("same.md", "private");
    let mut a = Reader::new(Limits::default()).unwrap();
    let mut b = Reader::new(Limits::default()).unwrap();
    let result_a = a.open(&path).unwrap().execute();
    let request_b = b.open(&path).unwrap();
    assert_eq!(b.apply(result_a), Apply::Stale);
    assert!(matches!(b.state(), ReaderState::Loading { .. }));
    assert!(b.document().is_none());
    assert!(matches!(b.apply(request_b.execute()), Apply::Ready(_)));
}

#[test]
fn unicode_invalidation_ranges_and_nonlocal_markdown_dependencies() {
    let fixture = Fixture::new();
    let path = fixture.file("doc.md", "a中尾\n\n[x][r]\n\n[r]: first\n");
    let mut reader = Reader::new(Limits::default()).unwrap();
    open(&mut reader, &path);
    fs::write(&path, "a中文尾\n\n[x][r]\n\n[r]: later\n").unwrap();
    let old = reader.document().unwrap().content.source.clone();
    let result = open(&mut reader, &path);
    let Apply::Ready(Update::Replaced {
        old: range_old,
        new: range_new,
    }) = result
    else {
        panic!("expected replacement");
    };
    let content = &reader.document().unwrap().content;
    assert!(old.get(range_old).is_some() && content.source.get(range_new).is_some());
    assert!(content
        .nodes
        .iter()
        .any(|n| matches!(&n.kind, NodeKind::Link { destination, .. } if destination == "later")));
}

#[cfg(windows)]
#[test]
fn windows_file_sharing_denial_is_reported_and_recoverable() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let path = fixture.file("locked.md", "locked");
    let handle = fs::OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    assert!(matches!(
        read_source(&path, Limits::default(), &Cancellation::default()),
        Err(ReaderError::Io { .. })
    ));
    drop(handle);
    assert!(read_source(&path, Limits::default(), &Cancellation::default()).is_ok());
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_for_files_and_parent_directories() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let path = fixture.file("doc.md", "local");
    let link = fixture.0.join("link.md");
    symlink(&path, &link).unwrap();
    assert_eq!(
        read_source(&link, Limits::default(), &Cancellation::default()).unwrap_err(),
        ReaderError::SymlinkOrReparsePoint
    );
    let directory_link = fixture.0.join("directory");
    symlink(&fixture.0, &directory_link).unwrap();
    assert_eq!(
        read_source(
            &directory_link.join("doc.md"),
            Limits::default(),
            &Cancellation::default()
        )
        .unwrap_err(),
        ReaderError::SymlinkOrReparsePoint
    );
}

#[test]
fn large_document_bounded_arena_and_drop() {
    let source = "## Section 世界\n\nText **strong** and $x$.\n\n".repeat(10_000);
    let content =
        parse_markdown(source, None, Limits::default(), &Cancellation::default()).unwrap();
    assert!(content.stats.nodes <= 100_000 && content.stats.max_depth <= 64);
    assert!(content.stats.payload_bytes <= 8 * 1024 * 1024);
    assert!(content.retained_bytes() < 32 * 1024 * 1024);
    drop(content);
    let too_deep = format!("{}text", "> ".repeat(1000));
    assert_eq!(
        parse_markdown(too_deep, None, Limits::default(), &Cancellation::default()).unwrap_err(),
        ReaderError::Limit(Resource::Depth)
    );
}
