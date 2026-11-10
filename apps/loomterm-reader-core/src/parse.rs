use crate::{
    Alignment, Cancellation, Content, ImageReference, Limits, Node, NodeKind, ParseStats,
    ReaderError, Resource,
};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use std::path::Path;

/// Consume owned UTF-8 into an independent AST. No resource reference performs I/O.
/// Cancellation is checked before construction and each event, not within parser internals.
pub fn parse_markdown(
    source: String,
    document: Option<&Path>,
    limits: Limits,
    cancel: &Cancellation,
) -> Result<Content, ReaderError> {
    parse_with_probe(source, document, limits, cancel, |_| {})
}

fn parse_with_probe(
    source: String,
    document: Option<&Path>,
    limits: Limits,
    cancel: &Cancellation,
    mut progress: impl FnMut(usize),
) -> Result<Content, ReaderError> {
    limits.validate()?;
    cancel.check()?;
    if source.len() > limits.source_bytes {
        return Err(ReaderError::Limit(Resource::SourceBytes));
    }
    if let Some(path) = document {
        if path.as_os_str().len() > crate::MAX_PATH_BYTES {
            return Err(ReaderError::Limit(Resource::PathBytes));
        }
        if !path.is_absolute() || !path.to_str().is_some_and(crate::image::is_local_path) {
            return Err(ReaderError::NonLocalPath);
        }
    }
    if limits.nodes == 0 {
        return Err(ReaderError::Limit(Resource::Nodes));
    }
    let bom = usize::from(source.starts_with('\u{feff}')) * 3;
    let mut nodes = vec![Node {
        kind: NodeKind::Document,
        span: 0..source.len(),
        parent: None,
        subtree_end: 1,
    }];
    let mut stack = vec![0];
    let mut stats = ParseStats {
        nodes: 1,
        ..ParseStats::default()
    };
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let mut events = Parser::new_ext(&source[bom..], options).into_offset_iter();
    loop {
        cancel.check()?;
        let Some((event, span)) = events.next() else {
            break;
        };
        progress(nodes.len());
        cancel.check()?;
        if matches!(event, Event::End(_)) {
            let id = stack.pop().ok_or(ReaderError::UnsupportedSyntax)?;
            nodes[id].subtree_end = nodes.len();
            continue;
        }
        if nodes.len() >= limits.nodes {
            return Err(ReaderError::Limit(Resource::Nodes));
        }
        // Depth includes leaves and excludes the synthetic Document root.
        let depth = stack.len();
        if depth > limits.depth {
            return Err(ReaderError::Limit(Resource::Depth));
        }
        let container = matches!(event, Event::Start(_));
        // Source is capped before parser creation. Charge all retained payload before insertion.
        let kind = match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => NodeKind::Paragraph,
                Tag::Heading { level, .. } => NodeKind::Heading(level as u8),
                Tag::BlockQuote(_) => NodeKind::BlockQuote,
                Tag::List(start) => NodeKind::List { start },
                Tag::Item => NodeKind::Item,
                Tag::CodeBlock(code) => NodeKind::CodeBlock {
                    info: match code {
                        CodeBlockKind::Indented => String::new(),
                        CodeBlockKind::Fenced(info) => info.into_string(),
                    },
                },
                Tag::HtmlBlock => NodeKind::HtmlBlock,
                Tag::Table(alignments) => NodeKind::Table {
                    alignments: alignments
                        .into_iter()
                        .map(|a| match a {
                            pulldown_cmark::Alignment::None => Alignment::None,
                            pulldown_cmark::Alignment::Left => Alignment::Left,
                            pulldown_cmark::Alignment::Center => Alignment::Center,
                            pulldown_cmark::Alignment::Right => Alignment::Right,
                        })
                        .collect(),
                },
                Tag::TableHead => NodeKind::TableHead,
                Tag::TableRow => NodeKind::TableRow,
                Tag::TableCell => NodeKind::TableCell,
                Tag::Emphasis => NodeKind::Emphasis,
                Tag::Strong => NodeKind::Strong,
                Tag::Strikethrough => NodeKind::Strikethrough,
                Tag::Link {
                    dest_url, title, ..
                } => NodeKind::Link {
                    destination: dest_url.into_string(),
                    title: title.into_string(),
                },
                Tag::Image {
                    dest_url, title, ..
                } => {
                    if stats.image_references >= limits.image_references {
                        return Err(ReaderError::Limit(Resource::ImageReferences));
                    }
                    stats.image_references += 1;
                    NodeKind::Image {
                        reference: ImageReference::resolve(&dest_url, document),
                        title: title.into_string(),
                    }
                }
                _ => return Err(ReaderError::UnsupportedSyntax),
            },
            Event::Text(text) => NodeKind::Text(text.into_string()),
            Event::Code(code) => NodeKind::Code(code.into_string()),
            Event::InlineMath(math) => NodeKind::Math {
                display: false,
                source: math.into_string(),
            },
            Event::DisplayMath(math) => NodeKind::Math {
                display: true,
                source: math.into_string(),
            },
            Event::Html(html) | Event::InlineHtml(html) => NodeKind::RawHtml(html.into_string()),
            Event::SoftBreak => NodeKind::SoftBreak,
            Event::HardBreak => NodeKind::HardBreak,
            Event::Rule => NodeKind::Rule,
            Event::TaskListMarker(checked) => NodeKind::TaskMarker(checked),
            _ => return Err(ReaderError::UnsupportedSyntax),
        };
        stats.payload_bytes = stats
            .payload_bytes
            .checked_add(payload(&kind))
            .ok_or(ReaderError::Limit(Resource::PayloadBytes))?;
        if stats.payload_bytes > limits.payload_bytes {
            return Err(ReaderError::Limit(Resource::PayloadBytes));
        }
        let id = nodes.len();
        nodes.push(Node {
            kind,
            span: span.start + bom..span.end + bom,
            parent: stack.last().copied(),
            subtree_end: id + 1,
        });
        if container {
            stack.push(id);
        }
        stats.max_depth = stats.max_depth.max(depth);
    }
    cancel.check()?;
    nodes[0].subtree_end = nodes.len();
    stats.nodes = nodes.len();
    Ok(Content {
        source,
        nodes,
        stats,
    })
}

fn payload(kind: &NodeKind) -> usize {
    match kind {
        NodeKind::Text(s)
        | NodeKind::Code(s)
        | NodeKind::RawHtml(s)
        | NodeKind::Math { source: s, .. }
        | NodeKind::CodeBlock { info: s } => s.capacity(),
        NodeKind::Link { destination, title } => destination.capacity() + title.capacity(),
        NodeKind::Image { reference, title } => reference.payload_bytes() + title.capacity(),
        NodeKind::Table { alignments } => alignments.capacity() * std::mem::size_of::<Alignment>(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_during_event_conversion() {
        let token = Cancellation::default();
        let source = "# Text **strong**\n\n".repeat(2000);
        let result = parse_with_probe(source, None, Limits::default(), &token, |nodes| {
            if nodes >= 16 {
                token.cancel();
            }
        });
        assert_eq!(result.unwrap_err(), ReaderError::Cancelled);
        assert!(parse_markdown(
            "after cancellation".into(),
            None,
            Limits::default(),
            &Cancellation::default()
        )
        .is_ok());
    }
}
