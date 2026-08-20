use smallvec::{SmallVec, smallvec};
use std::{borrow::Cow, collections::VecDeque, ops::Range};

use crate::{
    Action, Alignment, Event, ListKind, Options, Tag,
    inline::{InlineCursor, InlineParser, parse_inline},
    utils::bytes_has_nul,
};

pub struct BlockParser {
    /// Active container blocks
    containers: Vec<Container>,
    /// The currently open leaf block
    leaf: Option<Leaf>,
    options: Options,

    // Reused between parses to avoid allocation overhead.
    inline_parser: InlineParser<'static, 'static>,
}

impl BlockParser {
    pub fn new(options: Options) -> Self {
        Self {
            containers: Vec::new(),
            leaf: None,
            options,
            inline_parser: InlineParser::empty(),
        }
    }

    /// `line`: a `[start, end)` range representing line content w/o the line ending.
    ///         An exclusive end bound is necessary to represent empty lines (`\r`, `\n`, `\r\n`).
    /// `line_with_ending`: the same range extended through its complete line ending, if present.
    pub fn parse_line_for_iter<'a>(
        &mut self,
        buf: &'a [u8],
        line: Span,
        line_with_ending: Span,
        actions: &mut VecDeque<Action<'a>>,
        active: &mut Option<BufferedLeafEvents<'a>>,
    ) {
        let line_with_ending = &buf[line_with_ending];
        if self.containers.is_empty()
            && self.leaf_is_open()
            && self.handle_root_literals_fast_path(
                buf,
                line_with_ending,
                line.start,
                line.end - line.start,
                actions,
                active,
            )
        {
            // Cheaply deferred literal text is only possible when we are at root level (i.e. no containers). This
            // is because the premise of this fast-path optimization (that these leafs can be output line-by-line,
            // skipping `Scanner`) would no longer hold.
            return;
        }

        let bytes = &buf[line.clone()];
        let mut scanner = Scanner {
            bytes,
            full_line: line_with_ending,
            buf,
            line_offset: line.start,
            options: self.options,
            cursor: BlockCursor::new(bytes),
            content_start: 0,
            leaf: &mut self.leaf,
            containers: &mut self.containers,
            inline_parser: &mut self.inline_parser,
            actions,
            buffered: active,
        };
        scanner.parse_line();
    }

    /// Returns `true` if the line was fully handled. `false` means that the line was not *fully* parsed and that
    /// the `Scanner` must process what's left.
    fn handle_root_literals_fast_path<'a>(
        &mut self,
        buf: &'a [u8],
        line: &'a [u8],
        line_offset: usize,
        content_len: usize,
        actions: &mut VecDeque<Action<'a>>,
        active: &mut Option<BufferedLeafEvents<'a>>,
    ) -> bool {
        let bytes = &line[..content_len];
        let leaf = self.leaf.as_mut().unwrap();
        let mut saw_closer = false;

        // Whether lines get emitted as events right away or not. It drastically improves latency at the cost
        // of the parser possibly being a little bit slower from mutating the events queue more often.
        let skip_deferred = self.options.contains(Options::SKIP_ROOT_DEFERRED);

        // The line may be emitted verbatim IFF the line does not fulfill the closing condition for the
        // respective leaf.
        let handled = match leaf {
            Leaf::FencedCode {
                delim,
                len,
                indent,
                info: _,
                content,
                is_math,
            } => {
                // First, we need to check whether this is a closing fence. Closing fences can be preceded by
                // up to three spaces of indentationm.
                let mut cursor = BlockCursor::new(bytes);
                cursor.consume_many_space(3);

                if cursor.pending <= 3 && cursor.pos < bytes.len() && bytes[cursor.pos] == *delim {
                    let start = cursor.pos;

                    // Count the number of delimiters there are for the code fence. For it to be a closer,
                    // it *must* be >= `len` with **no** other non-whitespace character appearing on the line.
                    let mut end = start;
                    while end < bytes.len() && bytes[end] == *delim {
                        end += 1;
                    }

                    if end - start >= *len as usize && bytes[end..].iter().all(|&b| is_ws(b)) {
                        saw_closer = true;
                    }
                }

                if !saw_closer {
                    // Strip leading indentation from the content line, preserving any surplus.
                    let mut cursor = BlockCursor::new(bytes);
                    cursor.consume_many_space(*indent as u16);

                    let surplus = cursor.pending.saturating_sub(*indent as u16);
                    if skip_deferred {
                        let content_line = ContentLine::new(surplus, cursor.pos..line.len());
                        let content = content_line_to_cow(
                            line,
                            content_line,
                            bytes_has_nul(&line[cursor.pos..]),
                        );
                        let event = if *is_math {
                            Event::DisplayMath(content)
                        } else {
                            Event::Code(content)
                        };
                        actions.push_back(Action::Event(event));
                    } else {
                        content.push_contiguous(ContentLine::new(
                            surplus,
                            line_offset + cursor.pos..line_offset + line.len(),
                        ));
                    }
                }
                true
            }
            Leaf::Math {
                kind,
                indent,
                content,
            } => {
                // The same rules as for code fences.
                let mut cursor = BlockCursor::new(bytes);
                cursor.consume_many_space(3);

                if cursor.pending <= 3 && cursor.pos + 1 < bytes.len() {
                    let marker = &bytes[cursor.pos..cursor.pos + 2];
                    saw_closer = match kind {
                        DisplayMathKind::Dollars => marker == b"$$",
                        DisplayMathKind::Latex => marker == b"\\]",
                    } && bytes[cursor.pos + 2..].iter().all(|&b| is_ws(b));
                }

                if saw_closer {
                    true
                } else {
                    let mut cursor = BlockCursor::new(bytes);
                    cursor.consume_many_space(*indent as u16);

                    let surplus = cursor.pending.saturating_sub(*indent as u16);
                    if skip_deferred {
                        let content_line = ContentLine::new(surplus, cursor.pos..line.len());
                        let content = content_line_to_cow(
                            line,
                            content_line,
                            bytes_has_nul(&line[cursor.pos..]),
                        );
                        actions.push_back(Action::Event(Event::DisplayMath(content)));
                    } else {
                        content.push_contiguous(ContentLine::new(
                            surplus,
                            line_offset + cursor.pos..line_offset + line.len(),
                        ));
                    }
                    true
                }
            }
            Leaf::Html { kind, content } => {
                if kind.is_terminated_by(bytes) {
                    saw_closer = true;
                } else if skip_deferred {
                    // SAFETY: `line` is a subslice of the UTF-8 input passed to `Parser::feed`.
                    let text = unsafe { std::str::from_utf8_unchecked(line) };
                    let text = if bytes_has_nul(line) {
                        cow_nul(text)
                    } else {
                        Cow::Borrowed(text)
                    };
                    actions.push_back(Action::Event(Event::Html(text)));
                } else {
                    // TODO: make contiguous in the future.
                    content.push(line_offset..line_offset + line.len());
                }
                true
            }
            _ => false,
        };

        if !saw_closer {
            return handled;
        }

        // Emit close events!
        match self.leaf.take().unwrap() {
            Leaf::FencedCode {
                info,
                content,
                is_math,
                ..
            } => {
                if skip_deferred {
                    // CRITICAL: by this point, the opening line with `info` is no longer available (i.e. consumed)
                    // and the opener event has already been emitted.
                    actions.push_back(Action::Event(Event::End));
                    return true;
                }

                let (tag, kind) = if is_math {
                    (Tag::DisplayMath, BufferedLeafKind::DisplayMath)
                } else {
                    let tag = create_fenced_code_tag(buf, info);
                    (tag, BufferedLeafKind::Code)
                };
                *active = Some(BufferedLeafEvents::new(buf, tag, content, kind));
            }
            Leaf::Math { content, .. } => {
                if skip_deferred {
                    actions.push_back(Action::Event(Event::End));
                } else {
                    *active = Some(BufferedLeafEvents::new(
                        buf,
                        Tag::DisplayMath,
                        content,
                        BufferedLeafKind::DisplayMath,
                    ));
                }
            }
            Leaf::Html { kind, mut content } => {
                if !matches!(kind, HtmlKind::BlockTag | HtmlKind::CompleteTag) {
                    // We also need to include the closing line in the output unless it's type 6/7 where
                    // their end conditions (blank lines) shouldn't be in the output.
                    if skip_deferred {
                        // SAFETY: `line` is a subslice of the UTF-8 input passed to `Parser::feed`.
                        let text = unsafe { std::str::from_utf8_unchecked(line) };
                        let text = if bytes_has_nul(line) {
                            cow_nul(text)
                        } else {
                            Cow::Borrowed(text)
                        };
                        actions.push_back(Action::Event(Event::Html(text)));
                    } else {
                        content.push(line_offset..line_offset + line.len());
                    }
                }

                if skip_deferred {
                    actions.push_back(Action::Event(Event::End));
                } else {
                    actions.reserve(content.len() + 2);
                    actions.push_back(Action::Event(Event::Start(Tag::HtmlBlock)));
                    let may_have_nul = match (content.first(), content.last()) {
                        (Some(first), Some(last)) => bytes_has_nul(&buf[first.start..last.end]),
                        _ => false,
                    };
                    for span in content {
                        // SAFETY: `buf` is the UTF-8 input passed to `Parser::feed`.
                        let text = unsafe { &std::str::from_utf8_unchecked(buf)[span] };
                        let text = if may_have_nul {
                            cow_nul(text)
                        } else {
                            Cow::Borrowed(text)
                        };
                        actions.push_back(Action::Event(Event::Html(text)));
                    }
                    actions.push_back(Action::Event(Event::End));
                }
            }
            _ => unreachable!(),
        }

        true
    }

    pub fn leaf_is_open(&self) -> bool {
        self.leaf.is_some()
    }

    /// Shift source spans to match a feed buffer with `delta` bytes removed from their front.
    /// This is necessary so that the consumer can safely drop already consumed text. Otherwise, there
    /// would be out-of-bounds accesses.
    ///
    /// Spans must be shifted for cases in which a line may have simultaneously closed or emitted a leaf.
    // TODO: investigate whether we can do this on event emission to avoid shifting to begin with! Then, spans
    // would have the correct spans from the start. should be easy?
    pub fn update_leaf_spans(&mut self, delta: usize) {
        let skip_root_deferred =
            self.options.contains(Options::SKIP_ROOT_DEFERRED) && self.containers.is_empty();

        // TODO: can maybe consolidate spans/contentlines in future? just need fix HTML
        fn shift_spans(spans: &mut SmallVec<[Span; 4]>, delta: usize) {
            for span in spans {
                span.start -= delta;
                span.end -= delta;
            }
        }

        fn shift_content(content: &mut ContentLines, delta: usize) {
            for line in &mut content.lines {
                line.span.start -= delta;
                line.span.end -= delta;
            }
        }

        match self.leaf.as_mut() {
            Some(Leaf::Paragraph(spans, _)) => shift_spans(spans, delta),
            Some(Leaf::FencedCode { info, content, .. }) => {
                if !skip_root_deferred {
                    // These fields are ignored when skipping deferred output
                    info.start -= delta;
                    info.end -= delta;
                    shift_content(content, delta);
                }
            }
            Some(Leaf::IndentedCode(content)) => {
                shift_content(content, delta);
            }
            Some(Leaf::Math { content, .. }) if !skip_root_deferred => {
                shift_content(content, delta);
            }
            Some(Leaf::Math { .. }) => {}
            Some(Leaf::Html { content, .. }) => {
                if !skip_root_deferred {
                    // This field is ignored when skipping deferred output
                    shift_spans(content, delta);
                }
            }

            // Tables don't have any content; they're just a meta-container for table rows where events get
            // emitted immediately for parsed lines.
            Some(Leaf::Table { .. }) | None => {}
        }
    }

    pub fn finish_for_iter<'a>(
        &mut self,
        buf: &'a [u8],
        actions: &mut VecDeque<Action<'a>>,
        active: &mut Option<BufferedLeafEvents<'a>>,
    ) {
        let mut scanner = Scanner {
            bytes: &[],
            full_line: &[],
            buf,
            line_offset: 0,
            cursor: BlockCursor::new(&[]),
            content_start: 0,
            leaf: &mut self.leaf,
            containers: &mut self.containers,
            inline_parser: &mut self.inline_parser,
            options: self.options,
            actions,
            buffered: active,
        };
        scanner.close_leaf();
        scanner.close_containers(0);
    }

    #[inline(always)]
    pub fn next_inline_event<'a>(&mut self, cursor: &mut InlineCursor) -> Option<Event<'a>> {
        self.inline_parser.next_inline_event(cursor)
    }

    pub fn reset_inline(&mut self) {
        self.inline_parser.reset();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMathKind {
    Dollars,
    Latex,
}

fn create_fenced_code_tag<'a>(buf: &'a [u8], info: Span) -> Tag<'a> {
    // SAFETY: self.buf always comes from `s` in `feed()` thus is always valid UTF-8
    let mut info_str =
        crate::inline::unescape_string(unsafe { &std::str::from_utf8_unchecked(buf)[info] });

    // Must replace NUL per CommonMark specification
    if bytes_has_nul(info_str.as_bytes()) {
        info_str = info_str.replace('\0', "\u{FFFD}").into();
    }

    let tag = if info_str.is_empty() {
        Tag::CodeBlock(None)
    } else {
        Tag::CodeBlock(Some(info_str))
    };
    tag
}

// The `u8`s are delimiters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListDelimiter {
    /// `-`, `+` or `*`
    Bullet(u8),
    /// `.` or `)`
    Ordered(u8),
}

enum Container {
    Quote,
    List {
        delimiter: ListDelimiter,
    },
    Item {
        /// Required indentation for continuation, relative to its outer container.
        /// max(17) = marker_indent (0-3) + marker width (1-10) + whitespace (1-4)
        content_indent: u8,
        has_children: bool,
    },
}

// (start, end) byte indices
pub type Span = Range<usize>;

/// Per the specification, code blocks retain their leading indentation. Tabs must accordingly be
/// emitted as spaces.
struct ContentLine {
    /// The number of expanded spaces to add
    leading_virt_spaces: u16,
    /// The actual content
    span: Span,
}

struct ContentLines {
    lines: SmallVec<[ContentLine; 4]>,
}

impl ContentLines {
    fn new() -> Self {
        Self {
            lines: SmallVec::new(),
        }
    }

    fn push(&mut self, line: ContentLine) {
        self.lines.push(line);
    }

    fn push_contiguous(&mut self, line: ContentLine) {
        if line.leading_virt_spaces == 0
            && let Some(previous) = self.lines.last_mut()
            && previous.leading_virt_spaces == 0
            && previous.span.end == line.span.start
        {
            previous.span.end = line.span.end;
        } else {
            self.lines.push(line);
        }
    }

    fn first(&self) -> Option<&Span> {
        self.lines.first().map(|line| &line.span)
    }

    fn last(&self) -> Option<&Span> {
        self.lines.last().map(|line| &line.span)
    }

    fn pop(&mut self) -> Option<Span> {
        self.lines.pop().map(|line| line.span)
    }

    fn into_iter(self) -> ContentLineIter {
        self.lines.into_iter()
    }
}

type ContentLineIter = smallvec::IntoIter<[ContentLine; 4]>;

#[derive(Clone, Copy)]
enum BufferedLeafKind {
    Code,
    DisplayMath,
}

pub struct BufferedLeafEvents<'a> {
    buf: &'a [u8],
    start: Option<Tag<'a>>,
    lines: ContentLineIter,
    kind: BufferedLeafKind,
    may_have_nul: bool,
    end_pending: bool,
}

impl<'a> BufferedLeafEvents<'a> {
    fn new(buf: &'a [u8], start: Tag<'a>, lines: ContentLines, kind: BufferedLeafKind) -> Self {
        let may_have_nul = content_lines_may_have_nul(buf, &lines);
        Self {
            buf,
            start: Some(start),
            lines: lines.into_iter(),
            kind,
            may_have_nul,
            end_pending: true,
        }
    }
}

impl<'a> Iterator for BufferedLeafEvents<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(tag) = self.start.take() {
            return Some(Event::Start(tag));
        }
        if let Some(line) = self.lines.next() {
            let content = content_line_to_cow(self.buf, line, self.may_have_nul);
            return Some(match self.kind {
                BufferedLeafKind::Code => Event::Code(content),
                BufferedLeafKind::DisplayMath => Event::DisplayMath(content),
            });
        }
        self.end_pending.then(|| {
            self.end_pending = false;
            Event::End
        })
    }
}

impl ContentLine {
    fn new(leading_virt_spaces: u16, span: Span) -> Self {
        Self {
            leading_virt_spaces,
            span,
        }
    }
}

// NOTE: each vector represents content lines of a string (ignoring block prefixes etc.).
enum Leaf {
    /// (content, Option<(is_task, task_prefix_len_to_remove)>)
    Paragraph(SmallVec<[Span; 4]>, Option<(bool, usize)>),
    FencedCode {
        /// `` ` `` or `~`
        delim: u8,
        len: u16,
        indent: u8, // Always 0-3
        info: Span,
        content: ContentLines,
        is_math: bool,
    },
    IndentedCode(ContentLines),
    /// `$$ ... $$` or `\[ ... \]`
    Math {
        kind: DisplayMathKind,
        indent: u8, // Always 0-3
        content: ContentLines,
    },
    Html {
        kind: HtmlKind,
        content: SmallVec<[Span; 4]>,
    },
    Table {
        columns: u16,
        has_body: bool,
        cells: u32,
    },
}

const RAW_CONTENT_HTML_TAGS: &[&[u8]] = &[b"script", b"pre", b"style", b"textarea"];

#[derive(Clone, Copy)]
enum HtmlKind {
    RawTag(&'static [u8]),
    Comment,
    ProcessingInstruction,
    Declaration,
    Cdata,
    BlockTag,
    CompleteTag,
}

impl HtmlKind {
    fn is_terminated_by(self, bytes: &[u8]) -> bool {
        match self {
            // Might be faster to directly compare `</tag>`? micro-opt
            Self::RawTag(tag) => bytes.windows(tag.len() + 3).any(|window| {
                window.starts_with(b"</")
                    && window[2..window.len() - 1].eq_ignore_ascii_case(tag)
                    && window.ends_with(b">")
            }),
            Self::Comment => memchr::memmem::find(bytes, b"-->").is_some(),
            Self::ProcessingInstruction => memchr::memmem::find(bytes, b"?>").is_some(),
            Self::Declaration => memchr::memchr(b'>', bytes).is_some(),
            Self::Cdata => memchr::memmem::find(bytes, b"]]>").is_some(),
            Self::BlockTag | Self::CompleteTag => bytes.iter().all(|&b| b == b' ' || b == b'\t'),
        }
    }
}

/// The context from which [`Scanner::scan`] is called.
///
/// Paragraph: called from an existing paragraph leaf
/// NewBlock: called to make a new container/leaf
#[derive(Clone, Copy)]
enum Context<'a> {
    Paragraph {
        /// Whether the new line is possibly a lazy continuation.
        maybe_lazy: bool,
        /// GFM Table. Corresponds to a potential header line if the new line is parsed as a table
        /// delimiter line.
        prev_line_bytes: &'a [u8],
    },
    NewBlock,
}

// TODO: This can be eliminated? They correspond to branches anyway, I think, so seems redundant?
enum LineStart {
    /// All whitespace or line-ending-only
    Blank,
    /// Literal text
    Text,
    Setext(u8),
    /// Table delimiter row a vector of columns and their respective alignments to be rendered.
    Table(SmallVec<[Alignment; 4]>),
    Start(BlockStart),
}

/// A parsed block-start kind, ready to be applied by `apply_block_start`.
#[derive(Clone)]
enum BlockStart {
    Quote,
    ListItem {
        delimiter: ListDelimiter,
        /// Indentation of the list marker relative to the outer container.
        marker_indent: u8,
        /// max(17) = marker_indent (0-3) + marker width (1-10) + whitespace (1-4)
        content_indent: u8,
        /// Starting number for ordered lists.
        start: u32,
    },
    ATXHeading {
        level: u8,
        text: Span,
    },
    ThematicBreak,
    FencedCode {
        /// Either `` ` `` or `~`
        delim: u8,
        /// The number of delimiters used for the opening fence.
        len: u16,
        /// The fence's leading indentation relative to its outer container (0-3).
        /// Per specification, up to this amount of leading indentation must be trimmed from content
        /// lines inside the code block.
        indent: u8,
        /// The language the fenced code should be parsed as by the consumer, e.g. ```` ```rust ````
        info: Span,
    },
    DisplayMath {
        kind: DisplayMathKind,
        /// DisplayMath is valid IFF all content between the opener and closer are `>= indent`
        indent: u8,
    },
    HtmlBlock {
        kind: HtmlKind,
    },
    IndentedCode,
}

/// A per-line scanner invoked for block parsing
pub struct Scanner<'s, 'a: 's> {
    /// Bytes of the entire block that the current line is member to.
    buf: &'a [u8],

    /// Content bytes of the line currently being read, excluding its line ending.
    bytes: &'a [u8],

    /// Same as `bytes` but with the line ending (if any) attached.
    full_line: &'a [u8],

    /// The byte offset of `bytes` within `buf`.
    line_offset: usize,

    /// The current position within `bytes`.
    cursor: BlockCursor<'a>,

    /// The position of the first non-WS byte of the content span.
    /// Used for HTML block (and locally for text in `scan()`).
    // NOTE: Not needed and *can* be trivially reconstructed in `open_blocks()`, but it's cheaper
    // like this.
    content_start: usize,

    /// The currently active block. Events are emitted only when a leaf terminates (either by
    /// interruption or by satisfying completion conditions).
    leaf: &'s mut Option<Leaf>,

    /// All container blocks that were valid up to the previous line.
    /// Root container is implicit.
    containers: &'s mut Vec<Container>,

    // Preserved and reused to avoid allocations across the lifetime of the document being parsed.
    inline_parser: &'s mut InlineParser<'static, 'static>,
    options: Options,

    // Output
    actions: &'s mut VecDeque<Action<'a>>,
    buffered: &'s mut Option<BufferedLeafEvents<'a>>,
}

impl<'s, 'a: 's> Scanner<'s, 'a> {
    fn should_emit_event_immediately(&self) -> bool {
        self.options.contains(Options::SKIP_ROOT_DEFERRED) && self.containers.is_empty()
    }

    // This more-or-less follows the algorithm provided in the CommonMark specification's apendix.
    pub fn parse_line(&mut self) {
        let matched = self.match_containers();
        if self.handle_leaf(matched) {
            return;
        }

        self.open_blocks(matched, None);
    }

    /// Iterates through container block markers at the beginning of the line to determine which
    /// [`Container`]s are likely still open. Returns the index (`matched`) of the container that may
    /// be dropped to inform list parsing (sibling items v. child list). But, `matched` *might still*
    /// be a lazy continuation.
    ///
    /// **Example:**
    /// ```md
    ///   - Hello,
    /// world!
    /// ```
    ///
    /// becomes
    ///
    /// ```html
    /// <ul>
    ///   <li><p>Hello, world!</p></li>
    /// </ul>
    /// ```
    ///
    /// even though `world!` didn't satisfy the list item's indentation rule.
    pub fn match_containers(&mut self) -> usize {
        let mut i = 0;
        while i < self.containers.len() {
            match self.containers[i] {
                Container::Quote => {
                    // Quotes may have up to 3 spaces of leading indentation relative to their
                    // parent container.
                    while self.cursor.pos < self.bytes.len() && self.cursor.pending < 3 {
                        if !self.cursor.consume_space() {
                            break;
                        }
                    }

                    // Indentation must be followed by `>`
                    if self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] == b'>' {
                        self.cursor.pos += 1;
                        self.cursor.pending = 0; // Consume all indentation!

                        // May be followed by +1 whitespace (optional).
                        if self.cursor.pos < self.bytes.len()
                            && (self.cursor.pending > 0 || self.cursor.consume_space())
                        {
                            self.cursor.pending -= 1;
                        }
                    } else {
                        // Line does not continue this quote.
                        return i;
                    }
                }
                Container::Item {
                    content_indent,
                    has_children,
                } => {
                    if self.cursor.remaining_is_blank() {
                        if !has_children {
                            // A list item can begin with at most one blank line.
                            // Therefore, the current line is not part of the item.
                            return i;
                        }
                        // The current blank line may be a lazy continuation.
                    } else {
                        // Non-empty lines MUST be indented >= `content_indent` to be part of the
                        // list item.
                        let mut c = self.cursor.clone(); // Needed for backtracking
                        while c.pending < content_indent as u16
                            && c.pos < self.bytes.len()
                            && c.consume_space()
                        {}

                        if c.pending >= content_indent as u16 {
                            self.cursor = c;
                            self.cursor.pending -= content_indent as u16;
                        } else {
                            // Content is not part of the list item
                            return i;
                        }
                    }
                }

                // It's a meta-container, so it's existence is tied to the list items.
                // Thus, always accept.
                Container::List { .. } => {}
            }

            i += 1;
        }

        i
    }

    /// This function must be called after [`Scanner::match_containers`]. It will parse the new line
    /// as a potential continuation of the current leaf block OR as a closer/interruption.
    ///
    /// Returns `true` IFF the entire line has been parsed/consumed.
    pub fn handle_leaf(&mut self, matched: usize) -> bool {
        let Some(leaf) = self.leaf.take() else {
            return false;
        };

        // Whether the new line is a possible lazy continuation.
        let maybe_lazy = matched < self.containers.len();

        match leaf {
            Leaf::Paragraph(mut content, task) => {
                let header_span = content.last().unwrap();
                let mut header_end = header_span.end;
                if header_end > header_span.start
                    && matches!(self.buf[header_end - 1], b'\n' | b'\r')
                {
                    header_end -= 1;
                }
                // TODO: ensure spans never contain newlines so above is unnecessary?

                let ctx = Context::Paragraph {
                    maybe_lazy,
                    prev_line_bytes: &self.buf[header_span.start..header_end],
                };
                match self.scan(ctx) {
                    // A paragraph terminates once a blank line is reached.
                    LineStart::Blank => {
                        self.emit_paragraph(content, task);
                        self.close_containers(matched);
                    }
                    LineStart::Text => {
                        content.push(self.inline_content_span());
                        *self.leaf = Some(Leaf::Paragraph(content, task));
                    }
                    LineStart::Setext(level) => {
                        self.actions.reserve(3);
                        self.actions
                            .push_back(Action::Event(Event::Start(Tag::Heading(level))));
                        let root =
                            parse_inline(self.inline_parser, self.buf, &content, self.options);
                        self.actions.push_back(Action::InlineParse(root));
                        self.actions.push_back(Action::Event(Event::End));
                    }
                    LineStart::Table(alignments) => {
                        // The header line should not be emitted as part of the interrupted paragraph.
                        let header = content.pop().unwrap();
                        if !content.is_empty() {
                            self.emit_paragraph(content, task);
                        }

                        self.open_table(alignments, header);
                    }
                    LineStart::Start(kind) => {
                        // Close paragraph, then open new blocks.
                        // Cursor is already at content position.
                        self.emit_paragraph(content, task);
                        self.open_blocks(matched, Some(kind));
                    }
                }

                // We already call `open_blocks()` directly above
                true
            }
            Leaf::FencedCode {
                delim,
                len,
                indent,
                info,
                mut content,
                is_math,
            } => {
                // This contradicts the specification's statement that lazy continuation lines should be
                // treated as though they were properly indented, but this is `cmark`'s behavior. *shrug*
                //
                // **Example:**
                // ~~~md
                // - ```
                // hello
                // ```
                // ~~~
                //
                // should be `<ul></li><code>hello</code></li></ul>`
                // but instead it's `<ul><li><code></code></li><p>foo</p><code></code>`
                if maybe_lazy {
                    self.emit_fenced_code(info, content, is_math);
                    return false;
                }

                // Check if this is a closing fence; which must be *at least* as long as the opening fence
                // Need to rollback cursor if the check fails lest we lose indentation information as code
                // blocks must preserve indentation.
                let c = self.cursor.clone();
                self.cursor.consume_many_space(3);
                if self.cursor.pending <= 3
                    && self.cursor.pos < self.bytes.len()
                    && self.bytes[self.cursor.pos] == delim
                {
                    let mut count = 0;
                    let mut p = self.cursor.pos;
                    while p < self.bytes.len() && self.bytes[p] == delim {
                        count += 1;
                        p += 1;
                    }
                    // Rest of line must be whitespace.
                    let mut tail = p;
                    while tail < self.bytes.len() && is_ws(self.bytes[tail]) {
                        tail += 1;
                    }
                    if count >= len && tail >= self.bytes.len() {
                        self.emit_fenced_code(info, content, is_math);
                        return true;
                    }
                }
                self.cursor = c;

                // Need to strip leading indentation from content lines according to the opening
                // fence's leading indentation. This is to accurately render code indentation.
                self.cursor.consume_many_space(indent as u16);
                content.push(self.content_line(indent as u16));
                *self.leaf = Some(Leaf::FencedCode {
                    delim,
                    len,
                    indent,
                    info,
                    content,
                    is_math,
                });
                true
            }
            Leaf::IndentedCode(mut content) => {
                // See context at the top of Leaf::FencedCode for why this is the case.
                if maybe_lazy {
                    self.emit_indented_code(content);
                    return false;
                }

                // The line must be indented at least 4 spaces to be part of the indented code block
                // Save cursor for rollback so that HTML block preserves indentation later.
                let c = self.cursor.clone();
                self.cursor.consume_many_space(4);
                if self.cursor.remaining_is_blank() {
                    // Blank lines must have their whitespace preserved literally. They're not required
                    // to be indented 4 spaces.
                    content.push(self.content_line(4));
                    *self.leaf = Some(Leaf::IndentedCode(content));
                    return true;
                }

                if self.cursor.pending < 4 {
                    self.emit_indented_code(content);
                    self.cursor = c;
                    return false;
                }

                content.push(self.content_line(4));
                *self.leaf = Some(Leaf::IndentedCode(content));
                true
            }
            Leaf::Math {
                kind,
                indent,
                mut content,
            } => {
                // Logic is copied from codeblocks since they're similar.
                if maybe_lazy {
                    self.emit_display_math(content);
                    return false;
                }

                // May close IFF the line is a closer followed by nothing but WS.
                // Closer must be indented no more than 3 spaces.
                self.cursor.consume_many_space(4);
                if self.cursor.pending < 4 && self.cursor.pos + 1 < self.bytes.len() {
                    let maybe_marker = &self.bytes[self.cursor.pos..self.cursor.pos + 2];
                    let maybe_closer = match kind {
                        DisplayMathKind::Dollars => maybe_marker == b"$$",
                        DisplayMathKind::Latex => maybe_marker == b"\\]",
                    };

                    if maybe_closer && self.bytes[self.cursor.pos + 2..].iter().all(|&b| is_ws(b)) {
                        self.emit_display_math(content);
                        return true;
                    }
                }

                // Strip up to `indent` columns to preserve intent
                self.cursor.consume_many_space(indent as u16);
                content.push(self.content_line(indent as u16));
                *self.leaf = Some(Leaf::Math {
                    kind,
                    indent,
                    content,
                });
                true
            }
            Leaf::Html { kind, mut content } => {
                let stream = self.should_emit_event_immediately();

                // Not spec compliant but see context for this behavior in Leaf::FencedCode
                if maybe_lazy {
                    self.emit_html(content);
                    return false;
                }

                let remaining_bytes = &self.bytes[self.cursor.pos..];

                if kind.is_terminated_by(remaining_bytes) {
                    // Types 1–5: include the closing line; types 6–7: a blank line ends the block so don't include.
                    if !matches!(kind, HtmlKind::BlockTag | HtmlKind::CompleteTag) {
                        let span = self.content_span();
                        if stream {
                            self.emit_html_line(span);
                        } else {
                            content.push(span);
                        }
                    }
                    if stream {
                        self.actions.push_back(Action::Event(Event::End));
                    } else {
                        self.emit_html(content);
                    }
                    return true;
                }

                let span = self.content_span();
                if stream {
                    self.emit_html_line(span);
                } else {
                    content.push(span);
                }
                *self.leaf = Some(Leaf::Html { kind, content });
                true
            }
            Leaf::Table {
                columns,
                mut has_body,
                mut cells,
            } => {
                // https://github.com/github/cmark-gfm/blob/499789b49373bfa045d0e7547e5ee63444c77bca/extensions/table.c#L15
                const MAX_AUTOCOMPLETED_CELLS: u32 = 0x80000;

                // Not spec compliant but see context for this behavior in Leaf::FencedCode
                if maybe_lazy {
                    self.close_table(has_body);
                    return false;
                }

                // Check whether the table is being interrupted by a new block.
                let result = self.scan(Context::NewBlock);
                match result {
                    LineStart::Text => {}
                    LineStart::Blank => {
                        self.close_table(has_body);
                        return true;
                    }
                    LineStart::Start(kind) => {
                        self.close_table(has_body);
                        self.open_blocks(matched, Some(kind));
                        return true;
                    }
                    // Setext/Table are returned only w/ Context::Paragraph
                    _ => unreachable!(),
                }

                // Needs to be properly set for paragraph emission on failure!
                debug_assert_eq!(self.cursor.pos, self.content_start);

                // Skip any framing `|` (which doesn't affect parsing here)
                if self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] == b'|' {
                    self.cursor.pos += 1;
                }

                // Count cells in the row by scanning for unescaped `|` separators.
                let mut row_cells: u16 = 0;
                while self.cursor.pos < self.bytes.len() {
                    while self.cursor.pos < self.bytes.len() && is_ws(self.bytes[self.cursor.pos]) {
                        self.cursor.pos += 1;
                    }
                    if self.cursor.pos >= self.bytes.len() {
                        break;
                    }
                    row_cells += 1;

                    // Extra cells don't get rendered anyway.
                    if row_cells == columns {
                        break;
                    }

                    // Look for next pipe, skipping escaped bytes.
                    while self.cursor.pos < self.bytes.len() {
                        let b = self.bytes[self.cursor.pos];
                        if b == b'\\' {
                            self.cursor.pos += 2;
                            continue;
                        }

                        if b == b'|' {
                            self.cursor.pos += 1;
                            break;
                        }
                        self.cursor.pos += 1;
                    }

                    if self.cursor.pos >= self.bytes.len() {
                        break;
                    }
                }
                self.cursor.pos = self.content_start; // Reset in case failure

                // Rows must have at least 1 cell to be part of a table
                if row_cells == 0 {
                    self.close_table(has_body);
                    *self.leaf = Some(Leaf::Paragraph(smallvec![self.inline_content_span()], None));
                    return true;
                }

                cells += columns.saturating_sub(row_cells) as u32;

                // Short-circuit to avoid DOS (per GitHub)
                if cells > MAX_AUTOCOMPLETED_CELLS {
                    self.close_table(has_body);
                    *self.leaf = Some(Leaf::Paragraph(smallvec![self.inline_content_span()], None));
                    return true;
                }

                if !has_body {
                    self.actions
                        .push_back(Action::Event(Event::Start(Tag::TableBody)));
                    has_body = true;
                }
                self.emit_table_row(columns);
                *self.leaf = Some(Leaf::Table {
                    columns,
                    has_body,
                    cells,
                });
                true
            }
        }
    }

    /// Open blocks after containers have been (partially) matched and the previous leaf (if any) has been closed.
    ///
    /// `pending_start`: if a blockstart is already known (avoids a redundant `scan()` call)
    fn open_blocks(&mut self, matched: usize, mut pending_block: Option<BlockStart>) {
        // Necessary to determine if a newly constructed list item is a sibling, child, or dedent relative to
        // the previous list item (which is always `matched`).
        let prev_item_content_indent: Option<u8> = match (
            matched.checked_sub(1).and_then(|i| self.containers.get(i)),
            self.containers.get(matched),
        ) {
            (Some(Container::List { .. }), Some(Container::Item { content_indent, .. })) => {
                Some(*content_indent)
            }
            _ => None,
        };

        // No container `>= matched` is valid; safe to close.
        self.close_containers(matched);

        loop {
            let result = match pending_block.take() {
                Some(kind) => LineStart::Start(kind),
                None => self.scan(Context::NewBlock),
            };

            match result {
                LineStart::Blank => {
                    // Blank lines do not terminate list items
                    return;
                }
                LineStart::Text => {
                    // A paragraph always interrupts a list (a bare list can appear since any list item
                    // would've gotten popped in `close_containers()`)
                    if matches!(self.containers.last(), Some(Container::List { .. })) {
                        self.close_leaf();
                        self.actions.push_back(Action::Event(Event::End));
                        self.containers.pop();
                    }

                    // Necessary to detect task -- per spec: list's first block -> is para -> can be task
                    // list if has marker.
                    let is_first_item_block = self.mark_item_has_child();

                    let spans = smallvec![self.inline_content_span()];
                    let task = (is_first_item_block && self.options.contains(Options::TASK_LISTS))
                        .then(|| self.try_task_list_marker(&spans[0]))
                        .flatten();
                    *self.leaf = Some(Leaf::Paragraph(spans, task));
                    return;
                }
                LineStart::Start(kind) => {
                    // If the parent is a List and the new block is not a list item that would continue it,
                    // close it. New item is now relative to grandparent.
                    if let Some(Container::List {
                        delimiter: list_delim,
                    }) = self.containers.last()
                    {
                        let need_close_list = match &kind {
                            BlockStart::ListItem {
                                delimiter,
                                marker_indent,
                                ..
                            } => {
                                // For a list item to be part of the list, it must have the same delimiter and also
                                // be indented the same as the first itme in the list.
                                delimiter != list_delim
                                    || prev_item_content_indent
                                        .is_some_and(|indent| *marker_indent >= indent)
                            }
                            // Thematic break, ATX heading, etc. also close an exposed list.
                            _ => true,
                        };

                        if need_close_list {
                            // Close the list
                            self.close_leaf();
                            self.actions.push_back(Action::Event(Event::End));
                            self.containers.pop();
                        }
                    }

                    match self.create_block(kind) {
                        true => return, // Leaf opened; cannot contain more leafs per spec so return
                        false => {}     // Container opened
                    }
                }
                _ => return,
            }
        }
    }

    fn close_containers(&mut self, matched: usize) {
        while self.containers.len() > matched {
            self.close_leaf();
            self.actions.push_back(Action::Event(Event::End)); // closes the container's Start
            self.containers.pop();
        }
    }

    /// Determines what type of block is being opened, and reutrns `true` if it's a leaf or `false`
    /// if it's a container. Note: leafs cannot contain other leafs per the specification
    fn create_block(&mut self, kind: BlockStart) -> bool {
        self.mark_item_has_child();

        // TODO: maybe create a cursor here for cursor rollback (content_start) for just HTML?
        match kind {
            BlockStart::Quote => {
                self.actions
                    .push_back(Action::Event(Event::Start(Tag::Quote)));
                self.containers.push(Container::Quote);
                false
            }
            BlockStart::ListItem {
                delimiter,
                marker_indent: _,
                content_indent,
                start,
            } => {
                match self.containers.last() {
                    Some(Container::List { .. }) => {
                        // If the new parent container is an exposed `List`, kind is guaranteed to be a sibling
                        // because `open_blocks` would've already closed that `List` otherwise.
                        self.containers.push(Container::Item {
                            content_indent,
                            has_children: false,
                        });
                        self.actions
                            .push_back(Action::Event(Event::Start(Tag::Item)));
                        false
                    }
                    _ => {
                        // Creating a new list altogether
                        let list_kind = match delimiter {
                            ListDelimiter::Bullet(_) => ListKind::Unordered,
                            ListDelimiter::Ordered(_) => ListKind::Ordered(start),
                        };

                        // THOUGHT: can maybe reserve? but idk. after a few parses should not need
                        // to reserve anyway -- but reserve() would add extra inst that slower perf?
                        self.containers.push(Container::List { delimiter });
                        self.actions
                            .push_back(Action::Event(Event::Start(Tag::List(list_kind))));
                        self.containers.push(Container::Item {
                            content_indent,
                            has_children: false,
                        });
                        self.actions
                            .push_back(Action::Event(Event::Start(Tag::Item)));
                        false
                    }
                }
            }
            BlockStart::ATXHeading { level, text } => {
                self.actions
                    .push_back(Action::Event(Event::Start(Tag::Heading(level))));
                self.actions.reserve(3);
                if !text.is_empty() {
                    let root = parse_inline(self.inline_parser, self.buf, &[text], self.options);
                    self.actions.push_back(Action::InlineParse(root));
                }
                self.actions.push_back(Action::Event(Event::End));
                true
            }
            BlockStart::ThematicBreak => {
                self.actions.push_back(Action::Event(Event::ThematicBreak));
                true
            }
            BlockStart::FencedCode {
                delim,
                len,
                indent,
                info,
            } => {
                // SAFETY: self.buf always comes from `s` in `feed()` thus is always valid UTF-8
                let is_math = self.options.contains(Options::MATH_CODE)
                    && crate::inline::unescape_string(unsafe {
                        &std::str::from_utf8_unchecked(self.buf)[info.clone()]
                    }) == "math";

                // Per GitHub's special math, ```math gets parsed as a display math block.
                let tag = if is_math {
                    Tag::DisplayMath
                } else {
                    create_fenced_code_tag(self.buf, info.clone())
                };

                if self.should_emit_event_immediately() {
                    self.actions.push_back(Action::Event(Event::Start(tag)));
                }
                *self.leaf = Some(Leaf::FencedCode {
                    delim,
                    len,
                    indent,
                    info,
                    content: ContentLines::new(),
                    is_math,
                });
                true
            }
            BlockStart::DisplayMath { kind, indent } => {
                if self.should_emit_event_immediately() {
                    self.actions
                        .push_back(Action::Event(Event::Start(Tag::DisplayMath)));
                }
                *self.leaf = Some(Leaf::Math {
                    kind,
                    indent,
                    content: ContentLines::new(),
                });
                true
            }
            BlockStart::HtmlBlock { kind } => {
                let span = self.content_span();
                if self.should_emit_event_immediately() {
                    self.actions
                        .push_back(Action::Event(Event::Start(Tag::HtmlBlock)));
                    self.emit_html_line(span);
                    if !matches!(kind, HtmlKind::BlockTag | HtmlKind::CompleteTag)
                        && kind.is_terminated_by(&self.bytes[self.content_start..])
                    {
                        self.actions.push_back(Action::Event(Event::End));
                    } else {
                        *self.leaf = Some(Leaf::Html {
                            kind,
                            content: SmallVec::new(),
                        });
                    }
                    return true;
                }

                let mut content = SmallVec::new();
                content.push(span);
                if !matches!(kind, HtmlKind::BlockTag | HtmlKind::CompleteTag)
                    && kind.is_terminated_by(&self.bytes[self.content_start..])
                {
                    self.emit_html(content);
                } else {
                    *self.leaf = Some(Leaf::Html { kind, content });
                }
                true
            }
            BlockStart::IndentedCode => {
                // By this point `self.cursor.pos` has already consumed the 4 spaces of indentation needed
                // for `IndentedCode`. Stored in pending. Need subtract to only get content's surplus indent.
                let mut content = ContentLines::new();
                content.push(self.content_line(4));
                *self.leaf = Some(Leaf::IndentedCode(content));
                true
            }
        }
    }

    // -------------------------------------------------------------------------
    // ###                              Helpers                              ###
    // -------------------------------------------------------------------------

    /// Mark the any parent [`Container::Item`] as having seen its first child block.
    /// Returns `true` if this is the item's first child block.
    fn mark_item_has_child(&mut self) -> bool {
        if let Some(Container::Item { has_children, .. }) = self.containers.last_mut() {
            let is_first = !*has_children;
            *has_children = true;
            is_first
        } else {
            false
        }
    }

    /// Checks whether a [`Container::Item`] may be a child of a [`Container::List`] (sibling of prev items)
    /// or starts a new list altogether.
    fn list_item_extends_list(&self, maybe_lazy: bool) -> bool {
        maybe_lazy
            && self
                .containers
                .iter()
                .any(|c| matches!(c, Container::List { .. }))
    }

    /// Preserves indentation literally for lines (including when tabs are expanded into spaces).
    fn content_line(&self, stripped: u16) -> ContentLine {
        let surplus = self.cursor.pending.saturating_sub(stripped);
        ContentLine::new(surplus, self.content_span())
    }

    fn content_span(&self) -> Span {
        let start = self.line_offset + self.cursor.pos;
        start..self.line_offset + self.full_line.len()
    }

    fn inline_content_span(&self) -> Span {
        let mut span = self.content_span();
        if self.full_line.ends_with(b"\r\n") {
            span.end -= 1;
        }
        span
    }

    // -------------------------------------------------------------------------
    // ###                             Scanners                              ###
    // -------------------------------------------------------------------------

    // ENTRY for scanning (called for leaf and opening new blocks)!
    fn scan(&mut self, ctx: Context<'a>) -> LineStart {
        // Leading indentation for HTML MUST be outputted literally.
        let line_start = self.cursor.pos;

        // Only up to 4 spaces of indentation matter to distinguish an indented code block
        // from all other blocks.
        self.cursor.consume_many_space(4);

        if self.cursor.remaining_is_blank() {
            return LineStart::Blank;
        }

        if self.cursor.pending >= 4 {
            match ctx {
                Context::Paragraph { .. } => {
                    // An indented code block cannot interrupt a paragraph.
                    //
                    // Additionally, Paragraph rules state that all leading whitespace must be trimmed
                    // from any paragraph. We can short-circuit here as Text since we know it cannot be
                    // any other kind of block (as they demand no more than 3 spaces of indentation).
                    self.cursor.consume_many_space(u16::MAX);
                    return LineStart::Text;
                }
                Context::NewBlock => {
                    return LineStart::Start(BlockStart::IndentedCode);
                }
            }
        }

        // Save content-start cursor for if the result is just text (and for HTML).
        self.content_start = self.cursor.pos;

        let b = self.bytes[self.cursor.pos];
        self.cursor.pos += 1;

        let (is_paragraph, maybe_lazy, prev_line_bytes) = match ctx {
            Context::Paragraph {
                maybe_lazy,
                prev_line_bytes,
            } => (true, maybe_lazy, Some(prev_line_bytes)),
            Context::NewBlock => (false, false, None),
        };

        // Check if ordered list
        if b.is_ascii_digit() {
            let mut num: u32 = (b - b'0') as u32; // 32 bits is sufficient for the max of 9 digits
            let mut digit_count = 1;

            // Per CommonMark rules
            while self.cursor.pos < self.bytes.len()
                && self.bytes[self.cursor.pos].is_ascii_digit()
                && digit_count < 9
            {
                num = num * 10 + (self.bytes[self.cursor.pos] - b'0') as u32;
                self.cursor.pos += 1;
                digit_count += 1;
            }

            // Digits must be followed by either `.` or `)` delimiter
            let delimiter = match self.bytes.get(self.cursor.pos).copied() {
                Some(delimiter @ (b'.' | b')')) => delimiter,
                _ => {
                    self.cursor.pos = self.content_start;
                    return LineStart::Text;
                }
            };
            self.cursor.pos += 1;

            // The list marker's indentation relative to the outer container.
            let marker_indent = self.cursor.pending as u8;

            // Must be followed by space, tab, or end-of-line.
            if self.cursor.pos < self.bytes.len() && !self.cursor.consume_space() {
                self.cursor.pos = self.content_start;
                return LineStart::Text;
            }

            // May then be followed by any amount of further whitespace.
            self.cursor.consume_many_space(u16::MAX);

            let is_blank = self.cursor.pos >= self.bytes.len();

            // An ordered list may interrupt a *paragraph* iff:
            // - It starts at 1
            // - It is non-blank
            // Must also check if this is a sibling list item!
            // TODO: cache extends? and also needs to check up to `matched`?
            if is_paragraph && (is_blank || num != 1) && !self.list_item_extends_list(maybe_lazy) {
                self.cursor.pos = self.content_start;
                return LineStart::Text;
            }

            let marker_width = digit_count + 1; // digits + delimiter byte.
            let trailing_ws = self.cursor.pending - marker_indent as u16; // WS after delim

            // The amount of trailing whitespace considered part of the list item itself, rather than
            // the content that follows it. This is given by the CommonMark specification.
            let consumed_ws = if is_blank || trailing_ws >= 5 {
                1
            } else {
                trailing_ws
            };

            // Consume trailing whitespace for just the list item
            if is_blank {
                // Per paragraph rules, WS-only lines should be stripped anyway, so this is safe.
                self.cursor.pending = 0;
            } else {
                self.cursor.pending -= marker_indent as u16 + consumed_ws;
            }

            // Parsed a numbered list!
            return LineStart::Start(BlockStart::ListItem {
                delimiter: ListDelimiter::Ordered(delimiter),
                marker_indent,
                content_indent: marker_indent + marker_width + consumed_ws as u8,
                start: num,
            });
        }

        // NOTE: self.cursor.pending can be ignored without consequence when we reach fallback since
        // it doesn't matter for LineStart::Text.
        let speculative_result = match b {
            b'-' | b'*' | b'+' | b'_' => {
                self.try_setext_thematic_list_table(prev_line_bytes, maybe_lazy)
            }
            b'>' => {
                // Quotes may be followed by other containers on the *same* line. For example: `> >`
                // or `> -`.
                //
                // We must reset `self.cursor.pending` so that these nested elements are parsed
                // correctly (i.e. do not inherit this container's indentation). Otherwise, ` > >   -`
                // would incorrectly treat the list item as an indented codeblock!
                self.cursor.pending = 0;

                // Quote may be followed by optional whitespace.
                if self.cursor.pos < self.bytes.len() && self.cursor.consume_space() {
                    self.cursor.pending = self.cursor.pending.saturating_sub(1);
                }
                LineStart::Start(BlockStart::Quote)
            }
            b'#' => self.try_atx_heading(),
            b'`' | b'~' => self.try_code_fence(b),
            b'$' if self.cursor.peek(self.cursor.pos) == Some(b'$')
                && self.options.contains(Options::MATH_DOLLARS) =>
            {
                self.cursor.pos += 1;
                self.try_display_math(DisplayMathKind::Dollars)
            }
            b'\\'
                if self.cursor.peek(self.cursor.pos) == Some(b'[')
                    && self.options.contains(Options::MATH_LATEX) =>
            {
                self.cursor.pos += 1;
                self.try_display_math(DisplayMathKind::Latex)
            }
            b'<' => match self.try_html_block(matches!(ctx, Context::NewBlock) || maybe_lazy) {
                Some(kind) => {
                    // INVARIANT: pending/last_tab_start are irrelevant by here so don't need to update them
                    self.cursor.pos = line_start;
                    LineStart::Start(BlockStart::HtmlBlock { kind })
                }
                None => LineStart::Text,
            },
            // Per spec, setext interrupts a paragraph IFF it's not a lazy continuation.
            b'=' if !maybe_lazy && is_paragraph => {
                while self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] == b'=' {
                    self.cursor.pos += 1;
                }
                let mut tail = self.cursor.pos;

                // No non-whitespace character should appear
                while tail < self.bytes.len() && is_ws(self.bytes[tail]) {
                    tail += 1;
                }
                if tail >= self.bytes.len() {
                    return LineStart::Setext(1);
                }
                LineStart::Text
            }
            // We can only parse a table when a delimiter row interrupts a paragraph because other
            // rows are not required to have pipes.
            b'|' | b':'
                if !maybe_lazy && is_paragraph && self.options.contains(Options::TABLES) =>
            {
                // Reset for `try_table_delim`
                // TODO: maybe just pass the delim to the function directly instead?
                if b == b':' {
                    self.cursor.pos = self.content_start;
                }
                match self.try_table_delim(0, prev_line_bytes.unwrap()) {
                    Some(alignments) => LineStart::Table(alignments),
                    None => LineStart::Text,
                }
            }
            _ => LineStart::Text,
        };

        // Reset to start of text content
        if matches!(speculative_result, LineStart::Text) {
            // NOTE: This is not spec-compliant. The specification *clearly* states that
            // all lines in a paragraph are stripped of their leading whitespace. However,
            // `cmark`'s handling of lazy continuation contradicts that rule.
            //
            // **Example**
            // ```md
            // - `a
            //   b`
            // ```
            //
            // renders as `- a b`
            // versus
            //
            // ```md
            // - `a
            //  b`
            // ```
            // renders as `- a  b` (extra space from leading indentation!)
            //
            // A code span is necessary to demonstrate this bug because the inline soft
            // line break rule requires stripping leading and trailing whitespace from the
            // line otherwise whereas code spans are required to *keep* their whitespace.
            //
            // `cmark` is followed here for reference parser compliance.
            if maybe_lazy {
                self.cursor.pos = line_start;
            } else {
                self.cursor.pos = self.content_start;
            }
        }

        speculative_result
    }

    /// Parse `b'-'`, `b'*'`, `b'+'`, `b'_'` for thematic-break / list-marker / setext / table.
    fn try_setext_thematic_list_table(
        &mut self,
        prev_line_bytes: Option<&'a [u8]>,
        maybe_lazy: bool,
    ) -> LineStart {
        let is_paragraph = prev_line_bytes.is_some();
        let b = self.bytes[self.cursor.pos - 1];
        let marker_indent = self.cursor.pending as u8;

        // A blank list item may not interrupt a paragraph unless it's extending an existing list as a
        // sibling item.
        let blank_may_not_interrupt = is_paragraph && !self.list_item_extends_list(maybe_lazy);

        // Backtracking is unavoidable to correctly parse list items as parsing for setext/thematic break/table
        // mutates cursor state. We'd lose info on starting content bytes in line.
        let c = self.cursor.clone();
        let create_list_start = |this: &mut Self| {
            let is_blank = this.cursor.pos >= this.bytes.len();
            let trailing_ws = this.cursor.pending - marker_indent as u16;
            let consumed_ws = if is_blank || trailing_ws >= 5 {
                1
            } else {
                trailing_ws
            };

            if is_blank {
                this.cursor.pending = 0;
            } else {
                this.cursor.pending -= marker_indent as u16 + consumed_ws;
            }

            LineStart::Start(BlockStart::ListItem {
                delimiter: ListDelimiter::Bullet(b),
                marker_indent,
                content_indent: marker_indent + 1 + consumed_ws as u8,
                start: 0,
            })
        };

        match b {
            b'-' => {
                // List marker iff '-' is immediately followed by whitespace or EOL.
                let is_list = matches!(self.bytes.get(self.cursor.pos), Some(b' ' | b'\t') | None);

                // Count initial contiguous dashes (to disambiguate setext from thematic break).
                let mut contiguous = 1; // includes first '-' already consumed
                while self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] == b'-' {
                    contiguous += 1;
                    self.cursor.pos += 1;
                }

                // Continue scanning run characters.
                let mut total_dash = contiguous;
                let mut seen_ws = false;
                let mut not_table = false; // Tables are invalid if we see a space between `-`
                let mut reached_eol = true; // Did the line contain only dashes + whitespace?
                while self.cursor.pos < self.bytes.len() {
                    let c = self.bytes[self.cursor.pos];
                    match c {
                        b'-' => {
                            total_dash += 1;
                            if seen_ws {
                                not_table = true;
                            }
                        }
                        b' ' => {
                            seen_ws = true;
                            self.cursor.pending += 1;
                        }
                        b'\t' => {
                            seen_ws = true;
                            self.cursor.add_tab();
                        }
                        b'|' if is_paragraph && !maybe_lazy && !is_list => {
                            if self.options.contains(Options::TABLES) && !not_table {
                                if let Some(alignments) = self.try_table_delim(
                                    1,
                                    prev_line_bytes.expect("paragraph context has a table header"),
                                ) {
                                    return LineStart::Table(alignments);
                                }
                            }
                            return LineStart::Text;
                        }
                        b':' if is_paragraph && !maybe_lazy && !is_list => {
                            // `:` after whitespace is invalid (colons may not be separate from dashes).
                            if self.options.contains(Options::TABLES) && !seen_ws && !not_table {
                                if let Some(alignments) = self.try_table_delim(
                                    1,
                                    prev_line_bytes.expect("paragraph context has a table header"),
                                ) {
                                    return LineStart::Table(alignments);
                                }
                            }
                            return LineStart::Text;
                        }
                        _ => {
                            if !is_list {
                                return LineStart::Text;
                            }
                            reached_eol = false;
                            break;
                        }
                    }
                    self.cursor.pos += 1;
                }

                // Precedence: setext > thematic break > list marker
                // Read spec rules if needed
                if is_paragraph && !maybe_lazy && total_dash == contiguous && reached_eol {
                    return LineStart::Setext(2);
                } else if total_dash >= 3 && reached_eol {
                    return LineStart::Start(BlockStart::ThematicBreak);
                } else if is_list {
                    self.cursor = c;
                    self.cursor.consume_many_space(u16::MAX);
                    if self.cursor.pos >= self.bytes.len() {
                        if blank_may_not_interrupt {
                            return LineStart::Text;
                        }
                    }
                    return create_list_start(self);
                }
                LineStart::Text
            }

            b'*' => {
                // Thematic/list
                let is_list = matches!(self.bytes.get(self.cursor.pos), Some(b' ' | b'\t') | None);

                let mut total: u16 = 1; // Includes first '*' already consumed by `scan`.
                let mut reached_eol = true; // Did the line contain only '*' + whitespace?
                while self.cursor.pos < self.bytes.len() {
                    match self.bytes[self.cursor.pos] {
                        b'*' => total += 1,
                        b' ' => self.cursor.pending += 1,
                        b'\t' => self.cursor.add_tab(),
                        _ => {
                            if !is_list {
                                return LineStart::Text;
                            }
                            reached_eol = false;
                            break;
                        }
                    }
                    self.cursor.pos += 1;
                }

                // Precedence: thematic break > list marker
                if total >= 3 && reached_eol {
                    return LineStart::Start(BlockStart::ThematicBreak);
                } else if is_list {
                    self.cursor = c;
                    self.cursor.consume_many_space(u16::MAX);
                    if self.cursor.pos >= self.bytes.len() {
                        if blank_may_not_interrupt {
                            return LineStart::Text;
                        }
                    }
                    return create_list_start(self);
                }
                LineStart::Text
            }

            b'+' => {
                let is_list = matches!(self.bytes.get(self.cursor.pos), Some(b' ' | b'\t') | None);
                if !is_list {
                    // A bullet list is the only block item that starts with `*` anyway
                    return LineStart::Text;
                }

                self.cursor.consume_many_space(u16::MAX);
                if self.cursor.pos >= self.bytes.len() {
                    if blank_may_not_interrupt {
                        return LineStart::Text;
                    }
                }
                return create_list_start(self);
            }

            b'_' => {
                // Thematic break only
                let mut total: u16 = 1; // Includes first '_' already consumed by `scan`.
                let mut only_ws = true;
                while self.cursor.pos < self.bytes.len() {
                    match self.bytes[self.cursor.pos] {
                        b'_' => total += 1,
                        b' ' | b'\t' => {}
                        _ => {
                            only_ws = false;
                            break;
                        }
                    }
                    self.cursor.pos += 1;
                }

                if only_ws && total >= 3 {
                    return LineStart::Start(BlockStart::ThematicBreak);
                }
                LineStart::Text
            }

            // No delims except above ever get passed to this method
            _ => unreachable!(),
        }
    }

    fn try_table_delim(&mut self, phase_in: u8, header: &[u8]) -> Option<SmallVec<[Alignment; 4]>> {
        let mut phase = phase_in;
        let mut has_dash = phase != 0;
        let mut align = Alignment::None;
        let mut seen_pipe = false; // We can't have a `--` line be recongized as a table
        let mut alignments: SmallVec<[Alignment; 4]> = SmallVec::new();

        while self.cursor.pos < self.bytes.len() {
            let b = self.bytes[self.cursor.pos];
            match phase {
                // SEEK: `|` is framing pipe if no cells yet
                0 => match b {
                    b' ' | b'\t' => {}
                    b'|' => {
                        // Empty if we're starting at a leading `|`
                        if !alignments.is_empty() {
                            // Empty cells are not allowed
                            return None;
                        }
                        seen_pipe = true;
                    }
                    b':' => {
                        align = Alignment::Left;
                        phase = 1;
                    }
                    b'-' => {
                        has_dash = true;
                        phase = 1;
                    }
                    // No other character should appear!
                    _ => return None,
                },
                // SCANNING or TRAILING
                1 | 2 => match b {
                    b' ' | b'\t' => phase = 2,
                    b'|' => {
                        if !has_dash {
                            return None;
                        }
                        alignments.push(align);
                        has_dash = false;
                        align = Alignment::None;
                        seen_pipe = true;
                        phase = 0;
                    }
                    b'-' if phase == 1 => has_dash = true,
                    b':' if phase == 1 => {
                        align = if align == Alignment::Left {
                            Alignment::Center
                        } else {
                            Alignment::Right
                        };
                        phase = 2;
                    }
                    _ => return None,
                },
                _ => unreachable!(),
            }
            self.cursor.pos += 1;
        }

        // There may be a trailing cell without a framing pipe
        if has_dash && (seen_pipe || !matches!(align, Alignment::None)) {
            alignments.push(align);
        }

        if alignments.is_empty() {
            // Found no cells
            return None;
        }

        // Count header cells in the previous line.
        // NOTE: headers do NOT *need* `|` (but `|` MUST be used to separate cells`)
        let mut j = 0;

        // Skip optional leading framing pipe (plus any leading ws, e.g. `   |`).
        while j < header.len() && is_ws(header[j]) {
            j += 1;
        }
        if j < header.len() && header[j] == b'|' {
            j += 1;
        }

        let mut header_count = 0;
        loop {
            while j < header.len() && is_ws(header[j]) {
                j += 1;
            }
            if j >= header.len() {
                // No more cells found
                break;
            }
            header_count += 1;

            // Check if there might be a subsequent cell (i.e. `|` separator)
            while j < header.len() {
                if header[j] == b'\\' {
                    j += 1;
                    if j < header.len() {
                        // Skip escaped characters
                        j += 1;
                    }
                    continue;
                }
                if header[j] == b'|' {
                    break;
                }
                j += 1;
            }

            if j >= header.len() {
                break;
            }
            j += 1;
        }

        // Header and delimiter rows MUST have the same number of cells.
        if header_count == alignments.len() {
            Some(alignments)
        } else {
            None
        }
    }

    fn try_atx_heading(&mut self) -> LineStart {
        let mut level = 1;
        while self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] == b'#' && level < 6
        {
            self.cursor.pos += 1;
            level += 1;
        } // Cannot exceed 6 hashes

        // Must be followed by at least one space, tab, or end-of-line.
        if self.cursor.pos < self.bytes.len() && !is_ws(self.bytes[self.cursor.pos]) {
            return LineStart::Text;
        }

        // May be followed by whitespace (just need content start byte)
        while self.cursor.pos < self.bytes.len() && self.cursor.consume_space() {}

        // Going in reverse is more efficient here!
        let mut end = self.bytes.len();
        while end > self.cursor.pos && is_ws(self.bytes[end - 1]) {
            end -= 1;
        }

        let last_non_ws = end;
        while end > self.cursor.pos && self.bytes[end - 1] == b'#' {
            end -= 1;
        }

        if end < last_non_ws {
            if end == self.cursor.pos || is_ws(self.bytes[end - 1]) {
                while end > self.cursor.pos && is_ws(self.bytes[end - 1]) {
                    end -= 1;
                }
            } else {
                // The # is a literal (escaped or follows a non-ws char)
                end = last_non_ws;
            }
        }

        // Convert line-relative span to buffer-relative for `create_block`.
        LineStart::Start(BlockStart::ATXHeading {
            level,
            text: (self.line_offset + self.cursor.pos)..(self.line_offset + end),
        })
    }

    fn try_code_fence(&mut self, delim: u8) -> LineStart {
        let mut count = 1; // consume first fence char
        while self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] == delim {
            count += 1;
            self.cursor.pos += 1;
        }

        // Need at least 3 delimiters
        if count < 3 {
            return LineStart::Text;
        }

        // Backtick fences may not have info strings with backticks in them.
        if delim == b'`' && self.bytes[self.cursor.pos..].contains(&b'`') {
            return LineStart::Text;
        }

        // Info string is from self.cursor.pos to EOL (trailing whitespace trimmed).
        let mut info_end = self.bytes.len();
        while info_end > self.cursor.pos && is_ws(self.bytes[info_end - 1]) {
            info_end -= 1;
        }

        LineStart::Start(BlockStart::FencedCode {
            delim,
            len: count,
            indent: self.cursor.pending as u8, // Needed for removing beginning indentation on lines per spec
            info: (self.line_offset + self.cursor.pos)..(self.line_offset + info_end),
        })
    }

    fn try_display_math(&mut self, kind: DisplayMathKind) -> LineStart {
        // Trailing content must be whitespace only.
        if !self.cursor.remaining_is_blank() {
            return LineStart::Text;
        }

        // Display math
        LineStart::Start(BlockStart::DisplayMath { kind, indent: 0 })
    }

    fn try_task_list_marker(&self, span: &Span) -> Option<(bool, usize)> {
        let bytes = self.buf.get(span.start..span.end)?;
        let mut p = 0;

        // Leading spaces are allowed.
        while bytes.get(p).copied() == Some(b' ') {
            p += 1;
        }

        let marker_start = p;
        if bytes.get(p).copied() != Some(b'[') {
            return None;
        }

        let checked = match bytes.get(p + 1).copied() {
            Some(b' ' | b'\t') => false,
            Some(b'x' | b'X') => true,
            _ => return None,
        };

        // By now, it ought to be e.g. `[ ] Task list item`
        if bytes.get(p + 2).copied() != Some(b']') {
            return None;
        }

        // Must be followed by at least one whitespace before other content.
        if !matches!(bytes.get(p + 3).copied(), Some(b' ' | b'\t')) {
            return None;
        }
        Some((checked, marker_start + 4))
    }

    fn try_html_block(&mut self, allow_complete_tag: bool) -> Option<HtmlKind> {
        const BLOCK_ELEMENT_HTML_TAGS: &[&str] = &[
            "address",
            "article",
            "aside",
            "base",
            "basefont",
            "blockquote",
            "body",
            "caption",
            "center",
            "col",
            "colgroup",
            "dd",
            "details",
            "dialog",
            "dir",
            "div",
            "dl",
            "dt",
            "fieldset",
            "figcaption",
            "figure",
            "footer",
            "form",
            "frame",
            "frameset",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "head",
            "header",
            "hr",
            "html",
            "iframe",
            "legend",
            "li",
            "link",
            "main",
            "menu",
            "menuitem",
            "nav",
            "noframes",
            "ol",
            "optgroup",
            "option",
            "p",
            "param",
            "search",
            "section",
            "summary",
            "table",
            "tbody",
            "td",
            "tfoot",
            "th",
            "thead",
            "title",
            "tr",
            "track",
            "ul",
        ];

        if self.cursor.pos >= self.bytes.len() {
            return None;
        }

        // Types 2, 4, and 5 have mutually exclusive `!` prefixes.
        if self.bytes[self.cursor.pos] == b'!' {
            self.cursor.pos += 1; // consume !

            // type 2
            if self.bytes[self.cursor.pos..].starts_with(b"--") {
                return Some(HtmlKind::Comment);
            }

            // Type 5
            if self.bytes[self.cursor.pos..].starts_with(b"[CDATA[") {
                return Some(HtmlKind::Cdata);
            }

            // Type 4
            if self
                .bytes
                .get(self.cursor.pos)
                .is_some_and(u8::is_ascii_alphabetic)
            {
                return Some(HtmlKind::Declaration);
            }
            return None;
        }

        // Type 3
        if self.bytes[self.cursor.pos] == b'?' {
            return Some(HtmlKind::ProcessingInstruction);
        } // So, type 1, 6, 7 left to check (all elements)

        // Handle if it's a closing tag.
        let closing = self.bytes[self.cursor.pos] == b'/';
        if closing {
            self.cursor.pos += 1;
        }

        // Must be followed by a tagname. At least one ASCII letter as start
        if !self
            .bytes
            .get(self.cursor.pos)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            return None;
        }

        // Check for tagname
        let tag_start = self.cursor.pos;
        while self.cursor.pos < self.bytes.len()
            && (self.bytes[self.cursor.pos].is_ascii_alphanumeric()
                || self.bytes[self.cursor.pos] == b'-')
        {
            self.cursor.pos += 1;
        }
        let tag = &self.bytes[tag_start..self.cursor.pos];

        let raw_tag = if closing {
            None
        } else {
            RAW_CONTENT_HTML_TAGS
                .iter()
                .find(|candidate| tag.eq_ignore_ascii_case(candidate))
                .copied()
        };

        // Type 1 **needs** these. Sufficient for 6/7
        let tag_boundary = self.cursor.pos == self.bytes.len()
            || matches!(self.bytes[self.cursor.pos], b' ' | b'\t' | b'>');

        if let Some(tag) = raw_tag.filter(|_| tag_boundary) {
            // Type 1!
            return Some(HtmlKind::RawTag(tag));
        }

        let is_block_element = BLOCK_ELEMENT_HTML_TAGS
            .binary_search_by(|candidate| {
                candidate
                    .bytes()
                    .cmp(tag.iter().copied().map(|byte| byte.to_ascii_lowercase()))
            })
            .is_ok();

        if is_block_element && (tag_boundary || self.bytes[self.cursor.pos..].starts_with(b"/>")) {
            // Type 6!!
            return Some(HtmlKind::BlockTag);
        }

        // Type 7 does NOT allow raw content!
        if !allow_complete_tag || raw_tag.is_some() {
            return None;
        }

        // Spec is ambiguous but "complete open/close tag" prob just generally means any tag left over...

        // Whitespace scanner: spaces/tabs
        // Copy-pasted from inline parser's html parsing
        if closing {
            // Closing Tag
            self.cursor.consume_many_space(u16::MAX);
            if self.bytes.get(self.cursor.pos) != Some(&b'>') {
                return None;
            }
            self.cursor.pos += 1;
        } else {
            // Open tag
            let mut separated = false;
            loop {
                // Handle any trailing whitespace after tagname/previous attribute
                if !separated {
                    let ws_start = self.cursor.pos;
                    self.cursor.consume_many_space(u16::MAX);
                    separated = self.cursor.pos != ws_start;
                }

                if self.cursor.pos >= self.bytes.len() {
                    return None;
                }

                // Closed without further attributes? Either > or />
                if self.bytes[self.cursor.pos] == b'>' {
                    self.cursor.pos += 1;
                    break;
                }
                if self.bytes[self.cursor.pos..].starts_with(b"/>") {
                    self.cursor.pos += 2;
                    break;
                }

                // There must be another attribute then, which requires at least some whitespace after tagname
                if !separated {
                    return None;
                }
                separated = false;

                // Attribute name MUST start with one of these
                if !self.bytes[self.cursor.pos].is_ascii_alphabetic()
                    && !matches!(self.bytes[self.cursor.pos], b'_' | b':')
                {
                    return None;
                }
                while self.cursor.pos < self.bytes.len()
                    && (self.bytes[self.cursor.pos].is_ascii_alphanumeric()
                        || matches!(self.bytes[self.cursor.pos], b'_' | b'-' | b'.' | b':'))
                {
                    self.cursor.pos += 1;
                }

                // Optional attribute value
                let after_name = self.cursor.pos;
                self.cursor.consume_many_space(u16::MAX);

                if self.bytes.get(self.cursor.pos) != Some(&b'=') {
                    separated = self.cursor.pos != after_name;
                    continue;
                }

                self.cursor.pos += 1; // consume =
                self.cursor.consume_many_space(u16::MAX);

                if self.cursor.pos >= self.bytes.len() {
                    return None;
                }

                // Unquoted value?
                if !matches!(self.bytes[self.cursor.pos], b'"' | b'\'') {
                    let value_start = self.cursor.pos;
                    while self.cursor.pos < self.bytes.len()
                        && !matches!(
                            self.bytes[self.cursor.pos],
                            b' ' | b'\t' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`'
                        )
                    {
                        self.cursor.pos += 1;
                    }

                    // No valid value followed
                    if self.cursor.pos == value_start {
                        return None;
                    }
                } else {
                    // Single/double quoted?
                    let quote = self.bytes[self.cursor.pos];
                    self.cursor.pos += 1; // consume quote

                    while self.cursor.pos < self.bytes.len() && self.bytes[self.cursor.pos] != quote
                    {
                        self.cursor.pos += 1;
                    }

                    if self.cursor.pos >= self.bytes.len() {
                        return None;
                    }
                    self.cursor.pos += 1;
                }
            }
        }

        // Rest of line must be whitespace for a complete tag.
        while self.cursor.pos < self.bytes.len() && is_ws(self.bytes[self.cursor.pos]) {
            self.cursor.pos += 1;
        }

        if self.cursor.pos == self.bytes.len() {
            // Type 7!!!
            Some(HtmlKind::CompleteTag)
        } else {
            None
        }
    }

    // -------------------------------------------------------------------------
    // ###                           Leaf emission                           ###
    // -------------------------------------------------------------------------

    fn close_leaf(&mut self) {
        if let Some(leaf) = self.leaf.take() {
            match leaf {
                Leaf::Paragraph(spans, task) => self.emit_paragraph(spans, task),
                Leaf::FencedCode {
                    info,
                    content,
                    is_math,
                    ..
                } => self.emit_fenced_code(info, content, is_math),
                Leaf::IndentedCode(content) => self.emit_indented_code(content),
                Leaf::Math { content, .. } => self.emit_display_math(content),
                Leaf::Html { content, .. } => self.emit_html(content),
                Leaf::Table { has_body, .. } => self.close_table(has_body),
            }
        }
    }

    fn defer_buffered_leaf(&mut self, tag: Tag<'a>, content: ContentLines, kind: BufferedLeafKind) {
        debug_assert!(self.buffered.is_none());
        *self.buffered = Some(BufferedLeafEvents::new(self.buf, tag, content, kind));
    }

    fn emit_paragraph(&mut self, mut spans: SmallVec<[Span; 4]>, task: Option<(bool, usize)>) {
        let mut skip_spans = 0;
        if let Some((checked, consumed)) = task {
            // Need to strip the `[ ]` marker itself from the emitted text.
            if let Some(first) = spans.first_mut() {
                first.start += consumed;
                if first.start == first.end {
                    skip_spans = 1;
                }
            }
            self.actions
                .push_back(Action::Event(Event::TaskListMarker(checked)));
        }

        let content = &spans[skip_spans..];
        self.actions.reserve(3);
        self.actions
            .push_back(Action::Event(Event::Start(Tag::Paragraph)));

        if !content.is_empty() {
            let root = parse_inline(self.inline_parser, self.buf, content, self.options);
            self.actions.push_back(Action::InlineParse(root));
        }
        self.actions.push_back(Action::Event(Event::End));
    }

    fn emit_fenced_code(&mut self, info: Span, content: ContentLines, is_math: bool) {
        if self.should_emit_event_immediately() {
            self.actions.push_back(Action::Event(Event::End));
            return;
        }

        let (tag, kind) = if is_math {
            (Tag::DisplayMath, BufferedLeafKind::DisplayMath)
        } else {
            let tag = create_fenced_code_tag(self.buf, info);
            (tag, BufferedLeafKind::Code)
        };
        self.defer_buffered_leaf(tag, content, kind);
    }

    fn emit_indented_code(&mut self, mut content: ContentLines) {
        while content.last().is_some_and(|span| {
            self.buf[span.clone()]
                .iter()
                .all(|&b| b == b' ' || b == b'\t' || b == b'\n')
        }) {
            content.pop();
        }
        self.defer_buffered_leaf(Tag::CodeBlock(None), content, BufferedLeafKind::Code);
    }

    fn emit_display_math(&mut self, content: ContentLines) {
        if self.should_emit_event_immediately() {
            self.actions.push_back(Action::Event(Event::End));
            return;
        }

        self.defer_buffered_leaf(Tag::DisplayMath, content, BufferedLeafKind::DisplayMath);
    }

    fn emit_html_line(&mut self, span: Span) {
        // SAFETY: `buf` is the &str content passed in from `feed()` so it's always valid UTF-8.
        let text = unsafe { &std::str::from_utf8_unchecked(&self.buf)[span] };
        let text = if bytes_has_nul(text.as_bytes()) {
            cow_nul(text)
        } else {
            Cow::Borrowed(text)
        };
        self.actions.push_back(Action::Event(Event::Html(text)));
    }

    fn emit_html(&mut self, content: SmallVec<[Span; 4]>) {
        if self.should_emit_event_immediately() {
            self.actions.push_back(Action::Event(Event::End));
            return;
        }

        self.actions.reserve(content.len() + 2);
        self.actions
            .push_back(Action::Event(Event::Start(Tag::HtmlBlock)));
        let may_have_nul = match (content.first(), content.last()) {
            (Some(first), Some(last)) => bytes_has_nul(&self.buf[first.start..last.end]),
            _ => false,
        };

        for sp in content {
            // SAFETY: `buf` is the &str content passed in from `feed()` so it's always valid UTF-8.
            let text = unsafe { &std::str::from_utf8_unchecked(&self.buf)[sp] };
            let text = if may_have_nul {
                cow_nul(text)
            } else {
                Cow::Borrowed(text)
            };
            self.actions.push_back(Action::Event(Event::Html(text)));
        }
        self.actions.push_back(Action::Event(Event::End));
    }

    fn close_table(&mut self, has_body: bool) {
        if has_body {
            self.actions.push_back(Action::Event(Event::End)); // TableBody
        }
        self.actions.push_back(Action::Event(Event::End)); // Table
    }

    fn open_table(&mut self, alignments: SmallVec<[Alignment; 4]>, header: Span) {
        let columns = alignments.len() as u16;

        // FIXME(perf): fix this highly inefficient shit!
        let align_vec: Vec<Alignment> = alignments.iter().copied().collect();
        self.actions
            .push_back(Action::Event(Event::Start(Tag::Table(align_vec))));
        self.actions
            .push_back(Action::Event(Event::Start(Tag::TableHead)));
        self.actions
            .push_back(Action::Event(Event::Start(Tag::TableRow)));
        self.emit_table_cells(columns, header);
        self.actions.push_back(Action::Event(Event::End)); // TableRow
        self.actions.push_back(Action::Event(Event::End)); // TableHead
        *self.leaf = Some(Leaf::Table {
            columns,
            has_body: false,
            cells: 0,
        });
    }

    /// Emit a body table row: Start(TableRow), per-column cells, End(TableRow).
    fn emit_table_row(&mut self, columns: u16) {
        self.actions
            .push_back(Action::Event(Event::Start(Tag::TableRow)));
        let start = self.line_offset + self.cursor.pos;
        let end = self.line_offset + self.bytes.len();
        self.emit_table_cells(columns, start..end);
        self.actions.push_back(Action::Event(Event::End)); // TableRow
    }

    /// Parses a line as a table row.
    fn emit_table_cells(&mut self, columns: u16, row: Span) {
        let offset = row.start;
        let bytes = &self.buf[row];
        let mut p = 0;

        // Skip leading whitespace.
        while p < bytes.len() && is_ws(bytes[p]) {
            p += 1;
        }

        // Strip optional leading framing pipe.
        if p < bytes.len() && bytes[p] == b'|' {
            p += 1;
        }

        for _col in 0..columns {
            // Find the end of this cell (next unescaped `|` or EOL).
            let cell_start = p;
            while p < bytes.len() {
                if bytes[p] == b'\\' {
                    p += 1;
                    if p < bytes.len() {
                        p += 1;
                    }
                    continue;
                }
                if bytes[p] == b'|' {
                    break;
                }
                p += 1;
            }
            let cell_end = p;

            // Skip the `|` separator for the next cell.
            if p < bytes.len() {
                p += 1;
            }

            // Trim trailing whitespace
            let mut end = cell_end;
            while end > cell_start && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
                end -= 1;
            }

            // Trim leading whitespace.
            let mut start = cell_start;
            while start < end && is_ws(bytes[start]) {
                start += 1;
            }

            // Convert to buffer-relative span.
            let span = (offset + start)..(offset + end);
            self.actions.reserve(3);
            self.actions
                .push_back(Action::Event(Event::Start(Tag::TableCell)));

            if !span.is_empty() {
                let node_start = self.inline_parser.node_count();
                let root = parse_inline(self.inline_parser, self.buf, &[span], self.options);
                if memchr::memchr(b'|', bytes).is_some() {
                    self.inline_parser.unescape_table_pipes_from(node_start);
                }
                self.actions.push_back(Action::InlineParse(root));
            }
            self.actions.push_back(Action::Event(Event::End));
        }
    }
}

/// Returns `true` if any of the contiguous `lines` has a NUL `\0` character (needs to be replaced with 0xFFDD per spec).
fn content_lines_may_have_nul(buf: &[u8], lines: &ContentLines) -> bool {
    match (lines.first(), lines.last()) {
        (Some(first), Some(last)) => bytes_has_nul(&buf[first.start..last.end]),
        _ => false,
    }
}

/// Applies any leading whitespace or NUL handling to the string so that it's ready to be emitted
fn content_line_to_cow<'a>(buf: &'a [u8], line: ContentLine, may_have_nul: bool) -> Cow<'a, str> {
    // SAFETY: `buf` always comes from the valid str passed to `feed()`.
    // TODO: maybe switch parser tyoe from u8 to str slice to reduce unsafe?
    let text = unsafe { &std::str::from_utf8_unchecked(&buf)[line.span] };

    if line.leading_virt_spaces == 0 {
        if may_have_nul {
            cow_nul(text)
        } else {
            Cow::Borrowed(text)
        }
    } else {
        let mut owned = String::with_capacity(line.leading_virt_spaces as usize + text.len());
        owned.extend(std::iter::repeat_n(' ', line.leading_virt_spaces as usize));
        owned.push_str(text);

        if may_have_nul && bytes_has_nul(owned.as_bytes()) {
            Cow::Owned(owned.replace('\0', "\u{FFFD}"))
        } else {
            Cow::Owned(owned)
        }
    }
}

pub fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t')
}

// NOTE: needs to check for NUL again even though callers have maybe_nul, because span may not necessarily
// have it, and replace() always allocates new string!
pub fn cow_nul<'a>(s: &'a str) -> Cow<'a, str> {
    if bytes_has_nul(s.as_bytes()) {
        Cow::Owned(s.replace('\0', "\u{FFFD}"))
    } else {
        Cow::Borrowed(s)
    }
}

#[derive(Clone, Copy)]
struct BlockCursor<'a> {
    /// Bytes of the line currently being read.
    bytes: &'a [u8],
    /// The cursor's position within `bytes`.
    pos: usize,
    /// The amount of remaining (unconsumed) spaces from tabs or regular spaces
    pending: u16,
    /// The byte position of the previous tab stop. It is +1 the position of the last `\t`.
    prev_tab_stop: usize,
}

impl<'a> BlockCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            pending: 0,
            prev_tab_stop: 0,
        }
    }

    /// Advance past one whitespace byte, accumulating column count in `pending`.
    /// Returns `true` if a valid whitespace was consumed.
    fn consume_space(&mut self) -> bool {
        debug_assert!(self.pos < self.bytes.len());
        match self.bytes[self.pos] {
            b' ' => self.pending += 1,
            b'\t' => self.add_tab(),
            _ => return false,
        }
        self.pos += 1;
        true
    }

    /// Accumulate the tab expansion width for a `\t` at `self.pos`.
    fn add_tab(&mut self) {
        self.pending += 4 - ((self.pos - self.prev_tab_stop) % 4) as u16;
        self.prev_tab_stop = self.pos + 1;
    }

    /// Consume whitespace until `pending >= target` or non-whitespace is hit.
    /// Increments both `pending` *and* `pos` (to be one after the last consumed space).
    fn consume_many_space(&mut self, target: u16) -> bool {
        while self.pending < target && self.pos < self.bytes.len() {
            if !self.consume_space() {
                return false;
            }
        }
        self.pending >= target
    }

    fn remaining_is_blank(&self) -> bool {
        self.bytes[self.pos..].iter().all(|&b| is_ws(b))
    }

    fn peek(&self, pos: usize) -> Option<u8> {
        self.bytes.get(pos).copied()
    }
}
