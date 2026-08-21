use std::fmt::Write;

use crate::{Alignment, Event, ListKind, Options, Tag, filter_disallowed_html_into};

#[derive(Clone, Copy)]
enum OpenTag {
    Paragraph,
    Heading(u8),
    CodeBlock,
    CodeSpan,
    DisplayMath,
    HtmlBlock,
    BlockQuote,
    OrderedList,
    UnorderedList,
    Item,
    Emphasis,
    Strong,
    Strikethrough,
    Table,
    TableHead,
    TableBody,
    TableRow,
    TableCell,
    Link,
    Image,
}

impl From<&Tag<'_>> for OpenTag {
    fn from(tag: &Tag<'_>) -> Self {
        match tag {
            Tag::Paragraph => Self::Paragraph,
            Tag::Heading(level) => Self::Heading(*level),
            Tag::CodeBlock(_) => Self::CodeBlock,
            Tag::CodeSpan => Self::CodeSpan,
            Tag::DisplayMath => Self::DisplayMath,
            Tag::HtmlBlock => Self::HtmlBlock,
            Tag::Quote => Self::BlockQuote,
            Tag::List(ListKind::Ordered(_)) => Self::OrderedList,
            Tag::List(ListKind::Unordered) => Self::UnorderedList,
            Tag::Item => Self::Item,
            Tag::Emphasis => Self::Emphasis,
            Tag::Strong => Self::Strong,
            Tag::Strikethrough => Self::Strikethrough,
            Tag::Table(_) => Self::Table,
            Tag::TableHead => Self::TableHead,
            Tag::TableBody => Self::TableBody,
            Tag::TableRow => Self::TableRow,
            Tag::TableCell => Self::TableCell,
            Tag::Link { .. } => Self::Link,
            Tag::Image { .. } => Self::Image,
        }
    }
}

/// Accumulates HTML output from a stream of parser events.
pub struct HtmlWriter {
    output: String,
    stack: Vec<OpenTag>,
    options: Options,
    table_alignments: Vec<Alignment>,
    table_column: usize,

    /// For dealing with multi=level nesting of images where events must be interpreted as alt text.
    image_depth: usize,
    /// Deferred title for images to emitted after the `alt` from nested images gets finalized.
    image_title: Option<String>,
}

impl HtmlWriter {
    pub fn new() -> Self {
        Self::with_options(Options::empty())
    }

    pub fn with_options(options: Options) -> Self {
        Self {
            output: String::new(),
            stack: Vec::new(),
            options,
            table_alignments: Vec::new(),
            table_column: 0,
            image_depth: 0,
            image_title: None,
        }
    }

    /// Append a raw HTML fragment directly. Warning: no escaping is performed!
    pub fn push_text_raw(&mut self, html: &str) {
        self.output.push_str(html);
    }

    /// Append the HTML representation of an event.
    pub fn push_event(&mut self, event: &Event) {
        // Handle alt text if needed.
        if self.image_depth > 0 {
            self.handle_event_in_image(event);
            return;
        }
        match event {
            Event::Start(tag) => {
                self.open(tag);
                self.stack.push(tag.into());
            }
            Event::End => self.close(),
            Event::Text(s) => escape_text(&mut self.output, s),
            Event::Code(s) => {
                escape_text(&mut self.output, s);

                // Newlines should be added for the final newline (not mandated; just done to match cmark ref output).
                if matches!(self.stack.last(), Some(OpenTag::CodeBlock))
                    && !s.is_empty()
                    && !s.ends_with('\n')
                {
                    self.output.push('\n');
                }
            }
            Event::SoftBreak => self.output.push('\n'),
            Event::HardBreak => self.output.push_str("<br />\n"),
            Event::ThematicBreak => self.push_block("<hr />\n"),
            Event::Html(s) => {
                let mut is_trailing = false;

                // Check if we're currently at the root raw HTML node. If not, we must NOT add trailing newlines
                // to avoid breaking whitespace-sensitive nodes (e.g. `<textarea>`). We must also avoid doing so
                // to inline HTML as... it's supposed to be inline.
                if !matches!(self.stack.last(), Some(OpenTag::HtmlBlock)) {
                    let is_inline = self.stack.iter().any(|tag| {
                        matches!(
                            tag,
                            OpenTag::Paragraph | OpenTag::Heading(_) | OpenTag::TableCell
                        )
                    });

                    if !is_inline {
                        self.push_ensure_new_line();
                        is_trailing = true;
                    }
                }

                // Escape possibly dangerous HTML per GFM.
                if self.options.contains(Options::TAGFILTER) {
                    filter_disallowed_html_into(&mut self.output, s);
                } else {
                    self.output.push_str(s);
                }

                if is_trailing {
                    self.push_ensure_new_line();
                }
            }
            Event::TaskListMarker(checked) => {
                self.output.push_str("<input type=\"checkbox\"");
                if *checked {
                    self.output.push_str(" checked=\"\"");
                }
                self.output.push_str(" disabled=\"\" /> ");
            }
            Event::InlineMath(source) => {
                self.output.push_str("<span class=\"math math-inline\">");
                escape_text(&mut self.output, source);
                self.output.push_str("</span>");
            }
            Event::DisplayMath(source) => {
                escape_text(&mut self.output, source);
            }
        }
    }

    fn handle_event_in_image(&mut self, event: &Event) {
        match event {
            Event::Start(tag) => {
                if matches!(tag, Tag::Image { .. }) {
                    self.image_depth += 1;
                }
                self.stack.push(tag.into());
            }
            Event::End => {
                let tag = self.stack.pop();
                if matches!(tag, Some(OpenTag::Image)) {
                    self.image_depth -= 1;

                    // Put in the dang title after the nested images are handled (seriously, who decided
                    // to allow nested images in the first place? so stupid)
                    if self.image_depth == 0 {
                        self.output.push('"');
                        if let Some(title) = self.image_title.take() {
                            self.output.push_str(" title=\"");
                            escape_attr(&mut self.output, &title);
                            self.output.push('"');
                        }
                        self.output.push_str(" />");
                    }
                }
            }
            Event::Text(s) => escape_attr(&mut self.output, s),
            Event::Code(s) => escape_attr(&mut self.output, s),
            Event::SoftBreak | Event::HardBreak => self.output.push(' '),
            _ => {}
        }
    }

    /// Consume the writer and return the rendered HTML.
    pub fn into_string(self) -> String {
        self.output
    }

    /// Remove and return HTML produced since the previous drain for continued parsing. Does NOT reset
    /// the internally maintained document structure.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }

    /// Opens a new element based on [`Tag`].
    fn open(&mut self, tag: &Tag) {
        match tag {
            Tag::Paragraph => self.push_block("<p>"),
            Tag::Heading(level) => {
                self.push_block_with_trailing_line_ending(format_args!("<h{}>", level))
            }
            Tag::CodeBlock(info) => {
                self.push_block("<pre><code");
                if let Some(info) = info {
                    let lang = info.split_whitespace().next().unwrap_or("");
                    if !lang.is_empty() {
                        self.output.push_str(" class=\"language-");
                        escape_attr(&mut self.output, lang);
                        self.output.push('"');
                    }
                }
                self.output.push('>');
            }
            Tag::CodeSpan => self.output.push_str("<code>"),
            Tag::DisplayMath => self.output.push_str("<span class=\"math math-display\">"),
            Tag::HtmlBlock => {
                self.push_ensure_new_line();
            }
            Tag::Quote => self.push_block("<blockquote>\n"),
            Tag::List(kind) => match kind {
                ListKind::Ordered(start) if *start != 1 => self
                    .push_block_with_trailing_line_ending(format_args!(
                        "<ol start=\"{}\">\n",
                        start
                    )),
                ListKind::Ordered(_) => self.push_block("<ol>\n"),
                ListKind::Unordered => self.push_block("<ul>\n"),
            },
            Tag::Item => self.push_block("<li>"),
            Tag::Emphasis => self.output.push_str("<em>"),
            Tag::Strong => self.output.push_str("<strong>"),
            Tag::Strikethrough => self.output.push_str("<del>"),
            Tag::Table(alignments) => {
                self.table_alignments.clone_from(alignments);
                self.push_block("<table>\n");
            }
            Tag::TableHead => self.output.push_str("<thead>\n"),
            Tag::TableBody => self.output.push_str("<tbody>\n"),
            Tag::TableRow => {
                self.table_column = 0;
                self.output.push_str("<tr>\n");
            }
            Tag::TableCell => {
                let in_head = self
                    .stack
                    .iter()
                    .any(|tag| matches!(tag, OpenTag::TableHead));
                self.output.push_str(if in_head { "<th" } else { "<td" });
                match self
                    .table_alignments
                    .get(self.table_column)
                    .copied()
                    .unwrap_or(Alignment::None)
                {
                    Alignment::None => {}
                    Alignment::Left => self.output.push_str(" style=\"text-align: left\""),
                    Alignment::Center => self.output.push_str(" style=\"text-align: center\""),
                    Alignment::Right => self.output.push_str(" style=\"text-align: right\""),
                }
                self.output.push('>');
                self.table_column += 1;
            }
            Tag::Link { url, title } => {
                self.output.push_str("<a href=\"");
                escape_uri(&mut self.output, url);
                if let Some(title) = title {
                    self.output.push_str("\" title=\"");
                    escape_attr(&mut self.output, title);
                }
                self.output.push_str("\">");
            }
            Tag::Image { url, title } => {
                self.output.push_str("<img src=\"");
                escape_uri(&mut self.output, url);
                self.output.push_str("\" alt=\"");
                self.image_title = title.as_ref().map(ToString::to_string);
                self.image_depth = 1;
            }
        }
    }

    fn close(&mut self) {
        let Some(tag) = self.stack.pop() else {
            return;
        };
        match tag {
            OpenTag::Paragraph => self.output.push_str("</p>\n"),
            OpenTag::Heading(level) => {
                let _ = writeln!(self.output, "</h{}>", level);
            }
            OpenTag::CodeBlock => self.output.push_str("</code></pre>\n"),
            OpenTag::CodeSpan => self.output.push_str("</code>"),
            OpenTag::DisplayMath => self.output.push_str("</span>"),
            OpenTag::HtmlBlock => {
                self.push_ensure_new_line();
            }
            OpenTag::BlockQuote => self.push_block("</blockquote>\n"),
            OpenTag::OrderedList => self.output.push_str("</ol>\n"),
            OpenTag::UnorderedList => self.output.push_str("</ul>\n"),
            OpenTag::Item => self.output.push_str("</li>\n"),
            OpenTag::Emphasis => self.output.push_str("</em>"),
            OpenTag::Strong => self.output.push_str("</strong>"),
            OpenTag::Strikethrough => self.output.push_str("</del>"),
            OpenTag::Table => {
                self.push_ensure_new_line();
                self.table_alignments.clear();
                self.output.push_str("</table>\n");
            }
            OpenTag::TableHead => self.output.push_str("</thead>\n"),
            OpenTag::TableBody => self.output.push_str("</tbody>\n"),
            OpenTag::TableRow => self.output.push_str("</tr>\n"),
            OpenTag::TableCell => {
                let in_head = self
                    .stack
                    .iter()
                    .any(|tag| matches!(tag, OpenTag::TableHead));
                self.output
                    .push_str(if in_head { "</th>\n" } else { "</td>\n" });
            }
            OpenTag::Link => self.output.push_str("</a>"),
            // Can never happen as image end events are handled in handle_event_in_image
            OpenTag::Image => {}
        }
    }

    // NOTE: mdtext ALWAYS normalizes to LF i.e. `\n`
    fn push_block(&mut self, html: &str) {
        self.push_ensure_new_line();
        self.output.push_str(html);
    }

    fn push_block_with_trailing_line_ending(&mut self, args: std::fmt::Arguments) {
        self.push_ensure_new_line();
        let _ = self.output.write_fmt(args);
    }

    /// Not mandatory but only done to match cmark.
    fn push_ensure_new_line(&mut self) {
        // Checks that the output has a newline before we push.
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
    }
}

impl Default for HtmlWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Markdown still needs to be escaped to safely render for HTML.
fn escape_text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Escape text for use inside a double-quoted attribute value.
fn escape_attr(out: &mut String, s: &str) {
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

fn escape_uri(out: &mut String, s: &str) {
    // CREDIT: pulldown-cmark
    /// A LUT for what ASCII symbols do NOT need to be escaped in URIs (i.e. percent-encoded).
    #[rustfmt::skip]
    const URI_SAFE: [u8; 128] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 1, 0, 1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 0, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 1,
        0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 0,
    ];
    const HEX_CHARS: &[u8] = b"0123456789ABCDEF";

    let bytes = s.as_bytes();
    let mut mark = 0;
    for i in 0..bytes.len() {
        let c = bytes[i];
        if c < 0x80 && URI_SAFE[c as usize] == 1 {
            continue;
        }
        // Write the safe prefix up to this byte.
        if mark < i {
            out.push_str(&s[mark..i]);
        }
        match c {
            b'&' => out.push_str("&amp;"),
            b'\'' => out.push_str("&#x27;"),
            _ => {
                out.push('%');
                out.push(HEX_CHARS[(c >> 4) as usize] as char);
                out.push(HEX_CHARS[(c & 0xF) as usize] as char);
            }
        }
        mark = i + 1;
    }
    if mark < bytes.len() {
        out.push_str(&s[mark..]);
    }
}
