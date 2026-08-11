//! Safe, local Markdown preview rendering for the GUI.
//!
//! Parsing is performed from the active in-memory buffer. HTML is displayed as
//! literal text, and images are represented by their alt text and URL; preview
//! rendering never executes markup or fetches remote content.

use eframe::egui;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme::GuiTheme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading(u8),
    Code,
    ListItem,
    Rule,
}

#[derive(Debug, Clone, Default)]
struct InlineStyle {
    strong: usize,
    emphasis: usize,
    strikethrough: usize,
    code: usize,
    link: bool,
}

#[derive(Debug, Clone)]
struct Span {
    text: String,
    style: InlineStyle,
}

#[derive(Debug, Clone)]
struct Block {
    kind: BlockKind,
    spans: Vec<Span>,
    quoted: bool,
    list_marker: Option<String>,
    list_depth: usize,
}

/// Parsed preview cache keyed by document identity and buffer revision.
#[derive(Default)]
pub(crate) struct MarkdownPreview {
    key: Option<(u64, u64)>,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    next: u64,
    ordered: bool,
}

#[derive(Default)]
struct MarkdownBuilder {
    blocks: Vec<Block>,
    current: Option<Block>,
    style: InlineStyle,
    quote_depth: usize,
    lists: Vec<ListState>,
    link_url: Option<String>,
    image_url: Option<String>,
}

impl MarkdownBuilder {
    fn begin(&mut self, kind: BlockKind) {
        if self.current.is_none() {
            self.current = Some(Block {
                kind,
                spans: Vec::new(),
                quoted: self.quote_depth > 0,
                list_marker: None,
                list_depth: 0,
            });
        }
    }

    fn push(&mut self, text: impl Into<String>) {
        if self.current.is_none() {
            self.begin(BlockKind::Paragraph);
        }
        let text = text.into();
        if text.is_empty() {
            return;
        }
        if let Some(block) = self.current.as_mut() {
            block.spans.push(Span {
                text,
                style: self.style.clone(),
            });
        }
    }

    fn finish(&mut self) {
        if let Some(block) = self.current.take() {
            if block.kind == BlockKind::Rule || block.spans.iter().any(|span| !span.text.is_empty())
            {
                self.blocks.push(block);
            }
        }
    }

    fn start_item(&mut self) {
        self.finish();
        let depth = self.lists.len().saturating_sub(1);
        let prefix = if let Some(list) = self.lists.last_mut() {
            if list.ordered {
                let prefix = format!("{}. ", list.next);
                list.next += 1;
                prefix
            } else {
                "• ".to_string()
            }
        } else {
            "• ".to_string()
        };
        self.begin(BlockKind::ListItem);
        if let Some(block) = self.current.as_mut() {
            block.list_marker = Some(prefix.trim_end().to_string());
            block.list_depth = depth;
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn parse(markdown: &str) -> Vec<Block> {
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut builder = MarkdownBuilder::default();

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => builder.begin(BlockKind::Paragraph),
                Tag::Heading { level, .. } => {
                    builder.finish();
                    builder.begin(BlockKind::Heading(heading_level(level)));
                }
                Tag::BlockQuote(_) => builder.quote_depth += 1,
                Tag::CodeBlock(_) => {
                    builder.finish();
                    builder.begin(BlockKind::Code);
                    builder.style.code += 1;
                }
                Tag::List(start) => builder.lists.push(ListState {
                    next: start.unwrap_or(1),
                    ordered: start.is_some(),
                }),
                Tag::Item => builder.start_item(),
                Tag::Emphasis => builder.style.emphasis += 1,
                Tag::Strong => builder.style.strong += 1,
                Tag::Strikethrough => builder.style.strikethrough += 1,
                Tag::Link { dest_url, .. } => {
                    builder.style.link = true;
                    builder.link_url = Some(dest_url.into_string());
                }
                Tag::Image { dest_url, .. } => {
                    builder.image_url = Some(dest_url.into_string());
                }
                Tag::FootnoteDefinition(label) => {
                    builder.finish();
                    builder.begin(BlockKind::Paragraph);
                    builder.style.strong += 1;
                    builder.push(format!("[^{label}]: "));
                    builder.style.strong -= 1;
                }
                Tag::TableRow => {
                    builder.finish();
                    builder.begin(BlockKind::Paragraph);
                }
                Tag::TableCell => {
                    if builder
                        .current
                        .as_ref()
                        .is_some_and(|block| !block.spans.is_empty())
                    {
                        builder.push(" | ");
                    }
                }
                Tag::HtmlBlock
                | Tag::Table(_)
                | Tag::TableHead
                | Tag::MetadataBlock(_)
                | Tag::DefinitionList
                | Tag::DefinitionListTitle
                | Tag::DefinitionListDefinition => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if !matches!(
                        builder.current.as_ref().map(|b| b.kind),
                        Some(BlockKind::ListItem)
                    ) {
                        builder.finish();
                    }
                }
                TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Item | TagEnd::TableRow => {
                    builder.finish();
                    if tag == TagEnd::CodeBlock {
                        builder.style.code = builder.style.code.saturating_sub(1);
                    }
                }
                TagEnd::BlockQuote(_) => {
                    builder.quote_depth = builder.quote_depth.saturating_sub(1)
                }
                TagEnd::List(_) => {
                    builder.finish();
                    builder.lists.pop();
                }
                TagEnd::Emphasis => {
                    builder.style.emphasis = builder.style.emphasis.saturating_sub(1)
                }
                TagEnd::Strong => builder.style.strong = builder.style.strong.saturating_sub(1),
                TagEnd::Strikethrough => {
                    builder.style.strikethrough = builder.style.strikethrough.saturating_sub(1)
                }
                TagEnd::Link => {
                    builder.style.link = false;
                    if let Some(url) = builder.link_url.take() {
                        builder.push(format!(" ({url})"));
                    }
                }
                TagEnd::Image => {
                    if let Some(url) = builder.image_url.take() {
                        builder.push(format!(" [image: {url}]"));
                    }
                }
                TagEnd::FootnoteDefinition => builder.finish(),
                TagEnd::HtmlBlock
                | TagEnd::Table
                | TagEnd::TableHead
                | TagEnd::TableCell
                | TagEnd::MetadataBlock(_)
                | TagEnd::DefinitionList
                | TagEnd::DefinitionListTitle
                | TagEnd::DefinitionListDefinition => {}
            },
            Event::Text(text) => builder.push(text.into_string()),
            Event::Code(text) => {
                let previous = builder.style.code;
                builder.style.code += 1;
                builder.push(text.into_string());
                builder.style.code = previous;
            }
            Event::Html(html) | Event::InlineHtml(html) => builder.push(html.into_string()),
            Event::SoftBreak => builder.push(" "),
            Event::HardBreak => builder.push("\n"),
            Event::Rule => {
                builder.finish();
                builder.begin(BlockKind::Rule);
                builder.finish();
            }
            Event::TaskListMarker(checked) => builder.push(if checked { "☑ " } else { "☐ " }),
            Event::FootnoteReference(label) => builder.push(format!("[^{label}]")),
            Event::InlineMath(math) => builder.push(format!("${math}$")),
            Event::DisplayMath(math) => builder.push(format!("$${math}$$")),
        }
    }
    builder.finish();
    builder.blocks
}

fn layout_job(block: &Block, theme: &GuiTheme, wrap_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let size = match block.kind {
        BlockKind::Heading(1) => 32.0,
        BlockKind::Heading(2) => 26.0,
        BlockKind::Heading(3) => 22.0,
        BlockKind::Heading(4) => 19.0,
        BlockKind::Heading(5) => 17.0,
        BlockKind::Heading(_) => 16.0,
        BlockKind::Code => 14.0,
        _ => 16.0,
    };
    job.wrap.max_width = wrap_width.max(1.0);
    for span in &block.spans {
        let mut format = egui::TextFormat {
            font_id: egui::FontId::new(
                size,
                if span.style.code > 0 || block.kind == BlockKind::Code {
                    egui::FontFamily::Monospace
                } else {
                    egui::FontFamily::Proportional
                },
            ),
            color: if span.style.link {
                theme.accent
            } else {
                theme.text
            },
            line_height: Some(if matches!(block.kind, BlockKind::Heading(_)) {
                size * 1.25
            } else {
                size * 1.55
            }),
            italics: span.style.emphasis > 0,
            ..Default::default()
        };
        if span.style.strong > 0 && !matches!(block.kind, BlockKind::Heading(_)) {
            format.font_id.size += 0.75;
            format.color = theme.panel_text;
        }
        if span.style.strikethrough > 0 {
            format.strikethrough = egui::Stroke::new(1.0_f32, theme.dim_text);
        }
        if span.style.link {
            format.underline = egui::Stroke::new(1.0_f32, theme.accent);
        }
        if span.style.code > 0 && block.kind != BlockKind::Code {
            format.background = theme.input_bg;
        }
        job.append(&span.text, 0.0, format);
    }
    job
}

fn render_block_contents(ui: &mut egui::Ui, block: &Block, theme: &GuiTheme, content_width: f32) {
    match block.kind {
        BlockKind::Rule => {
            ui.separator();
        }
        BlockKind::Code => {
            let horizontal_margin = (content_width * 0.04).clamp(4.0, 14.0);
            egui::Frame::none()
                .fill(theme.input_bg)
                .inner_margin(egui::Margin::symmetric(horizontal_margin, 12.0))
                .rounding(6.0)
                .stroke(egui::Stroke::new(1.0_f32, theme.border))
                .show(ui, |ui| {
                    let width = ui.available_width();
                    ui.add(egui::Label::new(layout_job(block, theme, width)).wrap());
                });
        }
        BlockKind::ListItem => {
            ui.horizontal_top(|ui| {
                let indent = (block.list_depth as f32 * 22.0).min(content_width * 0.35);
                ui.add_space(indent);
                ui.label(
                    egui::RichText::new(block.list_marker.as_deref().unwrap_or("•"))
                        .size(16.0)
                        .color(theme.accent),
                );
                let width = ui.available_width();
                ui.add(egui::Label::new(layout_job(block, theme, width)).wrap());
            });
        }
        _ => {
            ui.add(egui::Label::new(layout_job(block, theme, content_width)).wrap());
        }
    }
}

fn render_block(ui: &mut egui::Ui, block: &Block, theme: &GuiTheme, content_width: f32) {
    if block.quoted {
        let horizontal_margin = (content_width * 0.05).clamp(4.0, 16.0);
        egui::Frame::none()
            .fill(theme.panel_bg)
            .inner_margin(egui::Margin::symmetric(horizontal_margin, 10.0))
            .stroke(egui::Stroke::new(1.0_f32, theme.accent))
            .rounding(4.0)
            .show(ui, |ui| {
                let width = ui.available_width();
                render_block_contents(ui, block, theme, width);
            });
    } else {
        render_block_contents(ui, block, theme, content_width);
    }
}

impl MarkdownPreview {
    pub fn needs_refresh(&self, key: (u64, u64)) -> bool {
        self.key != Some(key)
    }

    pub fn refresh(&mut self, key: (u64, u64), markdown: &str) {
        self.blocks = parse(markdown);
        self.key = Some(key);
    }

    /// Render the cached Markdown in a vertically scrolling, read-only view.
    pub fn render(&self, ui: &mut egui::Ui, document_id: u64, theme: &GuiTheme) {
        egui::ScrollArea::vertical()
            .id_salt(("markdown_preview_scroll", document_id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let available_width = ui.available_width();
                let minimum_padding = (available_width * 0.08).clamp(8.0, 24.0);
                let content_width = (available_width - minimum_padding * 2.0).clamp(1.0, 920.0);
                let side_padding = ((available_width - content_width) * 0.5).max(0.0);

                ui.horizontal_top(|ui| {
                    ui.add_space(side_padding);
                    ui.vertical(|ui| {
                        ui.set_min_width(content_width);
                        ui.set_max_width(content_width);
                        ui.add_space(28.0);
                        for block in &self.blocks {
                            let heading = matches!(block.kind, BlockKind::Heading(_));
                            if heading {
                                ui.add_space(10.0);
                            }
                            render_block(ui, block, theme, content_width);
                            if matches!(block.kind, BlockKind::Heading(1 | 2)) {
                                ui.add_space(5.0);
                                ui.separator();
                            }
                            ui.add_space(match block.kind {
                                BlockKind::Heading(1 | 2) => 15.0,
                                BlockKind::Heading(_) => 10.0,
                                BlockKind::ListItem => 3.0,
                                BlockKind::Code => 14.0,
                                _ => 10.0,
                            });
                        }
                        ui.add_space(40.0);
                    });
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_builds_headings_lists_and_code() {
        let blocks = parse("# Title\n\n- **bold** item\n\n```rust\nfn main() {}\n```\n");
        assert!(matches!(blocks[0].kind, BlockKind::Heading(1)));
        assert!(matches!(blocks[1].kind, BlockKind::ListItem));
        assert!(blocks[1].spans.iter().any(|span| span.style.strong > 0));
        assert_eq!(blocks[1].list_marker.as_deref(), Some("•"));
        assert!(matches!(blocks[2].kind, BlockKind::Code));
    }

    #[test]
    fn html_and_remote_images_remain_inert_text() {
        let blocks = parse("<script>alert('no')</script>\n\n![alt](https://example.com/a.png)");
        let text: String = blocks
            .iter()
            .flat_map(|block| block.spans.iter())
            .map(|span| span.text.as_str())
            .collect();
        assert!(text.contains("<script>"));
        assert!(text.contains("[image: https://example.com/a.png]"));
    }

    #[test]
    fn preview_renders_into_an_egui_frame() {
        let context = egui::Context::default();
        let mut preview = MarkdownPreview::default();
        preview.refresh(
            (1, 0),
            "# Rendered\n\n**Markdown** stays within a readable column even when the window is very wide. \
             This sentence is deliberately long enough to exercise wrapping.",
        );
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_400.0, 900.0),
            )),
            ..Default::default()
        };
        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                preview.render(ui, 1, &crate::theme::DARK);
            });
        });
        assert!(!output.shapes.is_empty());
        let text_shapes: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(!text_shapes.is_empty());
        assert!(text_shapes
            .iter()
            .all(|text| text.galley.rect.width() <= 920.5));
        assert!(text_shapes.iter().all(|text| text.pos.x >= 200.0));
    }

    #[test]
    fn nested_emphasis_and_footnote_labels_are_preserved() {
        let blocks = parse("*outer **inner** outer*\n\n[^one]: first\n\n[^two]: second");
        let paragraph = &blocks[0];
        assert!(paragraph
            .spans
            .iter()
            .filter(|span| span.text.contains("outer"))
            .all(|span| span.style.emphasis > 0));

        let text: String = blocks
            .iter()
            .flat_map(|block| block.spans.iter())
            .map(|span| span.text.as_str())
            .collect();
        assert!(text.contains("[^one]:"));
        assert!(text.contains("[^two]:"));
    }

    #[test]
    fn quoted_lists_retain_both_block_styles() {
        let blocks = parse("> - quoted item");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0].kind, BlockKind::ListItem));
        assert!(blocks[0].quoted);
        assert_eq!(blocks[0].list_marker.as_deref(), Some("•"));
    }

    #[test]
    fn preview_fits_inside_a_narrow_split_pane() {
        let context = egui::Context::default();
        let mut preview = MarkdownPreview::default();
        preview.refresh(
            (2, 0),
            "> - deeply nested preview content that must wrap instead of clipping",
        );
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(160.0, 500.0),
            )),
            ..Default::default()
        };
        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                preview.render(ui, 2, &crate::theme::DARK);
            });
        });
        let text_shapes: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert!(!text_shapes.is_empty());
        let bounds: Vec<_> = text_shapes
            .iter()
            .map(|text| (text.pos.x, text.pos.x + text.galley.rect.width()))
            .collect();
        assert!(
            text_shapes.iter().all(|text| {
                text.pos.x >= 0.0 && text.pos.x + text.galley.rect.width() <= 160.5
            }),
            "text bounds: {bounds:?}"
        );
    }
}
