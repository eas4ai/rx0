//! GitHub-flavoured Markdown preview with source line markers.
//!
//! Ports `markdown.go`: GFM tables, footnotes, strikethrough, task
//! lists, autolinks, raw HTML passthrough, GitHub-style heading slugs,
//! `data-line` on blocks, and fenced code highlighted with the code
//! view's token classes. Rendering is a custom pulldown-cmark event
//! walk so every tag shape stays under our control.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;
use std::ops::Range;

pub const MAX_MARKDOWN_BYTES: u64 = 4 << 20;

fn esc_text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

fn esc_attr(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
}

/// GitHub heading slugs: lower case, letters/digits/`_`/`-` kept, spaces
/// to `-`, the rest dropped, `section` when empty, `-1` suffixes for
/// repeats. Ports Go `headingIDs`.
fn github_slug(text: &str, seen: &mut HashMap<String, ()>) -> String {
    let mut id = String::new();
    for r in text.trim().chars() {
        if r.is_alphabetic() || r.is_numeric() || r == '_' || r == '-' {
            id.extend(r.to_lowercase());
        } else if r == ' ' {
            id.push('-');
        }
    }
    if id.is_empty() {
        id = "section".to_string();
    }
    if !seen.contains_key(&id) {
        seen.insert(id.clone(), ());
        return id;
    }
    let mut i = 1;
    loop {
        let cand = format!("{id}-{i}");
        if !seen.contains_key(&cand) {
            seen.insert(cand.clone(), ());
            return cand;
        }
        i += 1;
    }
}

enum Frame {
    Heading { line: usize },
    Image { dest: String, title: String },
    FootnoteDef { label: String },
    Code,
}

struct Hoist {
    frame: Frame,
    body: String,
    /// Unescaped text for heading slugs (markup excluded, like goldmark).
    plain: String,
}

struct Renderer {
    out: String,
    /// Byte offsets of `\n` in the source for `data-line` lookup.
    newlines: Vec<usize>,
    seen_ids: HashMap<String, ()>,
    /// Footnote label to number, in first-reference order.
    footnotes: HashMap<String, usize>,
    foot_order: Vec<String>,
    /// Rendered definition bodies by label.
    defs: HashMap<String, String>,
    hoists: Vec<Hoist>,
    /// Open link/image destinations; appended to slug text at End, in
    /// document order (goldmark counts them toward heading slugs).
    dest_stack: Vec<String>,
    /// Inside a table head (`th`) or body (`td`).
    table_head: bool,
    /// pulldown 0.10 wraps body rows in TableRow but not the head row.
    head_row_open: bool,
    aligns: Vec<Alignment>,
    col: usize,
    /// A task marker just fired: the next text regains its space.
    pending_space: bool,
    code_lang: String,
    code_fenced: bool,
    code_start_line: usize,
}

impl Renderer {
    fn line_of(&self, byte: usize) -> usize {
        self.newlines.partition_point(|&n| n < byte) + 1
    }

    fn text(&mut self, s: &str) {
        // A task marker just fired: restore the space pulldown swallowed.
        let pending = std::mem::replace(&mut self.pending_space, false);
        let spaced;
        let s = if pending && !s.starts_with([' ', '\t']) {
            spaced = format!(" {s}");
            spaced.as_str()
        } else {
            s
        };
        match self.hoists.last_mut() {
            // Fence content stays raw: the highlighter escapes it later.
            Some(Hoist {
                frame: Frame::Code,
                body,
                ..
            }) => body.push_str(s),
            Some(h) => {
                h.plain.push_str(s);
                esc_text(&mut h.body, s);
            }
            None => esc_text(&mut self.out, s),
        }
    }

    fn raw(&mut self, s: &str) {
        match self.hoists.last_mut() {
            Some(h) => h.body.push_str(s),
            None => self.out.push_str(s),
        }
    }

    /// Close one link/image scope, counting its destination toward the
    /// heading slug when inside one. Image and code hoists hold no slug
    /// text of their own, so the destination passes through them.
    fn pop_dest(&mut self) {
        if let Some(dest) = self.dest_stack.pop() {
            for h in self.hoists.iter_mut().rev() {
                match h.frame {
                    Frame::Image { .. } | Frame::Code => continue,
                    _ => {
                        h.plain.push_str(&dest);
                        break;
                    }
                }
            }
        }
    }

    /// Nested blocks (a list inside an item) start on a fresh line.
    fn ensure_fresh_line(&mut self) {
        let fresh = match self.hoists.last() {
            Some(h) => h.body.is_empty() || h.body.ends_with('\n'),
            None => self.out.is_empty() || self.out.ends_with('\n'),
        };
        if !fresh {
            self.raw("\n");
        }
    }

    fn render(mut self, events: Vec<(Event<'_>, Range<usize>)>) -> String {
        for (event, range) in events {
            self.event(event, range);
        }
        if !self.foot_order.is_empty() {
            self.out
                .push_str("<div class=\"footnotes\" role=\"doc-endnotes\">\n<hr>\n<ol>\n");
            for label in std::mem::take(&mut self.foot_order) {
                let n = self.footnotes[&label];
                let mut body = self.defs.remove(&label).unwrap_or_default();
                let backref = format!(
                    "<a href=\"#fnref:{n}\" class=\"footnote-backref\" role=\"doc-backlink\">&#x21a9;&#xfe0e;</a>"
                );
                if let Some(stripped) = body.strip_suffix("</p>\n") {
                    body = format!("{stripped}&#160;{backref}</p>\n");
                } else {
                    body.push_str(&format!("<p>&#160;{backref}</p>\n"));
                }
                self.out
                    .push_str(&format!("<li id=\"fn:{n}\">\n{body}</li>\n"));
            }
            self.out.push_str("</ol>\n</div>\n");
        }
        self.out
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(end) => self.end(end),
            Event::Text(s) => self.text(&s),
            Event::Code(s) => {
                // Code text counts toward heading slugs, like goldmark.
                if let Some(h) = self.hoists.last_mut() {
                    h.plain.push_str(&s);
                }
                let mut code = String::from("<code>");
                esc_text(&mut code, &s);
                code.push_str("</code>");
                self.raw(&code);
            }
            Event::Html(s) => {
                // Raw HTML passes through: READMEs lean on it.
                self.raw(&s);
                if !s.ends_with('\n') {
                    self.raw("\n");
                }
            }
            Event::InlineHtml(s) => self.raw(&s),
            Event::FootnoteReference(label) => {
                let next = self.footnotes.len() + 1;
                let n = *self.footnotes.entry(label.to_string()).or_insert(next);
                if !self.foot_order.iter().any(|l| l == label.as_ref()) {
                    self.foot_order.push(label.to_string());
                }
                self.raw(&format!(
                    "<sup id=\"fnref:{n}\"><a href=\"#fn:{n}\" class=\"footnote-ref\" role=\"doc-noteref\">{n}</a></sup>"
                ));
            }
            Event::SoftBreak => self.raw("\n"),
            // Unverified against goldmark (no fixture yet); GFM renders a break.
            Event::HardBreak => self.raw("<br />\n"),
            Event::Rule => self.raw("<hr>\n"),
            Event::TaskListMarker(checked) => {
                if checked {
                    self.raw("<input checked=\"\" disabled=\"\" type=\"checkbox\">");
                } else {
                    self.raw("<input disabled=\"\" type=\"checkbox\">");
                }
                // Pulldown swallows the space after `]`; goldmark keeps it.
                self.pending_space = true;
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        let line = self.line_of(range.start);
        match tag {
            Tag::Paragraph => self.raw(&format!("<p data-line=\"{line}\">")),
            Tag::Heading { .. } => {
                self.hoists.push(Hoist {
                    frame: Frame::Heading { line },
                    body: String::new(),
                    plain: String::new(),
                });
            }
            Tag::BlockQuote => self.raw(&format!("<blockquote data-line=\"{line}\">")),
            Tag::CodeBlock(kind) => {
                let (fenced, lang) = match &kind {
                    CodeBlockKind::Fenced(info) => (
                        true,
                        info.split_whitespace().next().unwrap_or("").to_string(),
                    ),
                    CodeBlockKind::Indented => (false, String::new()),
                };
                self.code_fenced = fenced;
                self.code_lang = lang;
                self.code_start_line = line;
                self.hoists.push(Hoist {
                    frame: Frame::Code,
                    body: String::new(),
                    plain: String::new(),
                });
            }
            Tag::HtmlBlock => {}
            Tag::List(first) => {
                self.ensure_fresh_line();
                if first.is_some() {
                    self.raw(&format!("<ol data-line=\"{line}\">\n"));
                } else {
                    self.raw(&format!("<ul data-line=\"{line}\">\n"));
                }
            }
            Tag::Item => self.raw(&format!("<li data-line=\"{line}\">")),
            Tag::FootnoteDefinition(label) => {
                self.hoists.push(Hoist {
                    frame: Frame::FootnoteDef {
                        label: label.to_string(),
                    },
                    body: String::new(),
                    plain: String::new(),
                });
            }
            Tag::Table(aligns) => {
                self.aligns = aligns.to_vec();
                self.table_head = true;
                self.head_row_open = false;
                self.raw(&format!("<table data-line=\"{line}\">\n<thead>\n"));
            }
            Tag::TableHead => {}
            Tag::TableRow => {
                self.col = 0;
                self.raw("<tr>\n");
            }
            Tag::TableCell => {
                if self.table_head && !self.head_row_open {
                    self.head_row_open = true;
                    self.raw("<tr>\n");
                }
                let align = self
                    .aligns
                    .get(self.col)
                    .copied()
                    .unwrap_or(Alignment::None);
                self.col += 1;
                let style = match align {
                    Alignment::None => String::new(),
                    Alignment::Left => " style=\"text-align:left\"".to_string(),
                    Alignment::Center => " style=\"text-align:center\"".to_string(),
                    Alignment::Right => " style=\"text-align:right\"".to_string(),
                };
                let tag = if self.table_head { "th" } else { "td" };
                self.raw(&format!("<{tag}{style}>"));
            }
            Tag::Emphasis => self.raw("<em>"),
            Tag::Strong => self.raw("<strong>"),
            Tag::Strikethrough => self.raw("<del>"),
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => {
                self.dest_stack.push(dest_url.to_string());
                let mut s = String::from("<a href=\"");
                if matches!(link_type, pulldown_cmark::LinkType::Email) {
                    s.push_str("mailto:");
                }
                esc_attr(&mut s, &dest_url);
                s.push('"');
                if !title.is_empty() {
                    s.push_str(" title=\"");
                    esc_attr(&mut s, &title);
                    s.push('"');
                }
                s.push('>');
                self.raw(&s);
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                self.dest_stack.push(dest_url.to_string());
                self.hoists.push(Hoist {
                    frame: Frame::Image {
                        dest: dest_url.to_string(),
                        title: title.to_string(),
                    },
                    body: String::new(),
                    plain: String::new(),
                });
            }
            Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => self.raw("</p>\n"),
            TagEnd::Heading(level) => {
                let Some(Hoist {
                    frame: Frame::Heading { line },
                    body,
                    plain,
                }) = self.hoists.pop()
                else {
                    return;
                };
                let slug = github_slug(&plain, &mut self.seen_ids);
                self.raw(&format!(
                    "<{level} id=\"{slug}\" data-line=\"{line}\">{body}</{level}>\n"
                ));
            }
            TagEnd::BlockQuote => self.raw("</blockquote>\n"),
            TagEnd::CodeBlock => {
                let Some(Hoist { body: code, .. }) = self.hoists.pop() else {
                    return;
                };
                let lang = std::mem::take(&mut self.code_lang);
                let mut s = String::from("<pre class=\"md-code\"");
                if !code.is_empty() {
                    // The fence marker line holds no code: data-line is the
                    // first content line, exactly like goldmark's segments.
                    let content_line = if self.code_fenced {
                        self.code_start_line + 1
                    } else {
                        self.code_start_line
                    };
                    s.push_str(&format!(" data-line=\"{content_line}\""));
                }
                if !lang.is_empty() {
                    s.push_str(" data-lang=\"");
                    esc_attr(&mut s, &lang);
                    s.push('"');
                }
                s.push_str("><code>");
                s.push_str(&crate::highlight::highlight_code_block(&code, &lang));
                s.push_str("</code></pre>\n");
                self.raw(&s);
            }
            TagEnd::HtmlBlock => {}
            TagEnd::List(ordered) => self.raw(if ordered { "</ol>\n" } else { "</ul>\n" }),
            TagEnd::Item => self.raw("</li>\n"),
            TagEnd::FootnoteDefinition => {
                if let Some(Hoist {
                    frame: Frame::FootnoteDef { label },
                    body,
                    ..
                }) = self.hoists.pop()
                {
                    if !label.is_empty() {
                        self.defs.insert(label, body);
                    }
                }
            }
            TagEnd::Table => {
                self.raw("</tbody>\n</table>\n");
            }
            TagEnd::TableHead => {
                self.table_head = false;
                self.raw("</tr>\n</thead>\n<tbody>\n");
            }
            TagEnd::TableRow => self.raw("</tr>\n"),
            TagEnd::TableCell => self.raw(if self.table_head {
                "</th>\n"
            } else {
                "</td>\n"
            }),
            TagEnd::Emphasis => self.raw("</em>"),
            TagEnd::Strong => self.raw("</strong>"),
            TagEnd::Strikethrough => self.raw("</del>"),
            TagEnd::Link => {
                self.pop_dest();
                self.raw("</a>");
            }
            TagEnd::Image => {
                self.pop_dest();
                if let Some(Hoist {
                    frame: Frame::Image { dest, title },
                    body: alt,
                    ..
                }) = self.hoists.pop()
                {
                    let mut s = String::from("<img src=\"");
                    esc_attr(&mut s, &dest);
                    s.push_str("\" alt=\"");
                    esc_attr(&mut s, &alt);
                    s.push('"');
                    if !title.is_empty() {
                        s.push_str(" title=\"");
                        esc_attr(&mut s, &title);
                        s.push('"');
                    }
                    s.push('>');
                    self.raw(&s);
                }
            }
            TagEnd::MetadataBlock(_) => {}
        }
    }
}

/// Render Markdown to preview HTML. Ports Go `renderMarkdown`.
pub fn render_markdown(src: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(src).replace("\r\n", "\n");
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&text, opts);
    let events: Vec<(Event<'_>, Range<usize>)> = parser.into_offset_iter().collect();
    let mut newlines = Vec::new();
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            newlines.push(i);
        }
    }
    let renderer = Renderer {
        out: String::with_capacity(text.len() * 2),
        newlines,
        seen_ids: HashMap::new(),
        footnotes: HashMap::new(),
        foot_order: Vec::new(),
        defs: HashMap::new(),
        hoists: Vec::new(),
        dest_stack: Vec::new(),
        table_head: false,
        head_row_open: false,
        aligns: Vec::new(),
        col: 0,
        pending_space: false,
        code_lang: String::new(),
        code_fenced: false,
        code_start_line: 0,
    };
    Ok(renderer.render(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> String {
        render_markdown(src.as_bytes()).unwrap()
    }

    #[test]
    fn headings_slug_like_github() {
        let html =
            render("# Getting Started!\n\n## snake_case API\n\n## ???\n\n# Getting Started\n");
        assert!(html.contains("<h1 id=\"getting-started\" data-line=\"1\">Getting Started!</h1>"));
        assert!(html.contains("<h2 id=\"snake_case-api\""));
        assert!(html.contains("<h2 id=\"section\""));
        assert!(html.contains("<h1 id=\"getting-started-1\""));
    }

    #[test]
    fn fence_keeps_shape_and_highlights() {
        let html = render("```go\nfunc main() {}\n```\n");
        assert!(html.contains("<pre class=\"md-code\" data-line=\"2\" data-lang=\"go\"><code>"));
        // Sublime scopes `func` as storage.type (Chroma: Keyword), so it
        // takes the type class. Theme-compatible, not token-identical.
        assert!(html.contains("<i class=kt>func</i>"));
        assert!(html.contains("<i class=nf>main</i>"));
        let plain = render("```\nplain\n```\n");
        assert!(plain.contains("<pre class=\"md-code\" data-line=\"2\"><code>plain</code></pre>"));
    }

    #[test]
    fn task_list_and_table_shapes() {
        let html = render("- [x] done\n- [ ] todo\n");
        assert!(html.contains("<input checked=\"\" disabled=\"\" type=\"checkbox\"> done"));
        assert!(html.contains("<input disabled=\"\" type=\"checkbox\"> todo"));
        let table = render("| a |\n|---|\n| 1 |\n");
        assert!(table.contains("<table data-line=\"1\">\n<thead>\n<tr>\n<th>a</th>\n</tr>"));
    }
}
