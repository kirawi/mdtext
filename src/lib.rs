mod block;
mod generated_entities;
mod inline;
mod tagfilter;
mod utils;

pub mod html;

use std::borrow::Cow;
use std::collections::VecDeque;

/// A parse event, yielded by [`Parser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// Signals the beginning of a new markdown block or inline element.
    Start(Tag<'a>),
    /// Signals that the most recently opened element has just closed (i.e. last-in first-out).
    End,
    /// A span of text within the currently active element. It is **never** multi-line.
    Text(Cow<'a, str>),
    /// A span of code content within the currently active code span/block.
    Code(Cow<'a, str>),
    /// A line break that must be interpreted in HTML as either a literal `\n` or as a `U+0020` space.
    /// It may only appear after a `Text` element.
    SoftBreak,
    /// A line break that must be interpreted in HTML as `<br />`. It may only appear after a `Text`
    /// element.
    HardBreak,
    /// Must be interpreted in HTML as `<hr />`.
    ThematicBreak,
    /// A soan of HTML within the current block or inline HTML.
    /// Should be emitted literally.
    Html(Cow<'a, str>),
    /// Marker for a task-list item. `true` means checked. May only appear on list items.
    TaskListMarker(bool),
    /// GitHub/LaTeX math syntax (`$...$`, `` $`...`$ ``, or `\(...\)`).
    InlineMath(Cow<'a, str>),
    /// A span of display math content within [`Tag::DisplayMath`].
    DisplayMath(Cow<'a, str>),
}

/// The kind of element that is being opened. See: [`Event::Start`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag<'a> {
    /// `<p>...</p>`
    Paragraph,
    /// `<h1>...</h1>` from 1-6 levels (same as HTML).
    Heading(u8),
    /// Either an indented or fenced code block with an optional info string.
    /// The info string should be included in HTML as a `class="language-XXX"` atribute.
    CodeBlock(Option<Cow<'a, str>>),
    /// An inline `<code>...</code>` span.
    CodeSpan,
    /// A GitHub/LaTeX display-math construct.
    DisplayMath,
    /// A CommonMark §4.6 HTML block.
    HtmlBlock,
    /// §5.1 Block quotes.
    Quote,
    /// Either a `<ul>` or `<ol>` depending on [`ListKind`]
    List(ListKind),
    /// An `<li>` within the current list.
    Item,
    /// Emphasis (usually interpreted as italic)
    Emphasis,
    /// Strong emphasis (usually interpreted as bold).
    Strong,
    /// A destination `url` and an optional (i.e. empty == `None`) title.
    Link {
        url: Cow<'a, str>,
        title: Option<Cow<'a, str>>,
    },
    /// A destination `url` and an optional (i.e. empty == `None`) title.
    /// Description content will be emitted as child events until [`Event::End`].
    Image {
        url: Cow<'a, str>,
        title: Option<Cow<'a, str>>,
    },

    // ------------------
    // ### Extensions ###
    // ------------------
    /// `<del>...</del>`. Requires [`Options::STRIKETHROUGH`]
    Strikethrough,
    /// `<td>...</td>`. Requires [`Options::TABLES`]
    Table(Vec<Alignment>),
    /// `<th>...</th>`. Requires [`Options::TABLES`]
    TableHead,
    /// `<tbody>...</tbody>`. Requires [`Options::TABLES`]
    TableBody,
    /// `<tr>...</tr>`. Requires [`Options::TABLES`]
    TableRow,
    /// Signals the start of content contained within a table cell. Requires [`Options::TABLES`]
    TableCell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    /// Numbered list with the `u32` field representing the starting item number.
    Ordered(u32),
    /// Ordered list
    Unordered,
}

/// Text alignment of all cells within the currently active table column. Returned only for the table head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

pub(crate) enum Action<'a> {
    Event(Event<'a>),
    /// Drain one detached root from the reusable inline-parser arena.
    InlineParse(inline::InlineRoot),
}

// NOTE: Left as u32 for future expansion
/// Bitflag parser extensions. Example: [`Options::GFM`] enables GitHub-flavored markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Options(u32);

impl Options {
    /// GFM Tables extension.
    pub const TABLES: Self = Self(1 << 0);
    /// GFM Task list items extension.
    pub const TASK_LISTS: Self = Self(1 << 1);
    /// GFM Strikethrough extension.
    pub const STRIKETHROUGH: Self = Self(1 << 2);
    /// GFM Autolinks extension.
    pub const EXTENDED_AUTOLINKS: Self = Self(1 << 3);
    /// GFM Disallowed Raw HTML extension (tagfilter).
    pub const TAGFILTER: Self = Self(1 << 4);
    /// GitHub math syntax: `$...$` and `$$...$$`.
    pub const MATH_DOLLARS: Self = Self(1 << 5);
    /// GitHub math syntax: `` $`...`$ ``.
    pub const MATH_CODE: Self = Self(1 << 6);
    /// LaTeX math syntax: `\(...\)` inline and `\[...\]` display.
    pub const MATH_LATEX: Self = Self(1 << 7);
    /// CommonMark and GFM diverge in classifying emphasis runs due to differing definitions of
    /// Unicode punctuation and how emphasis runs are minimized. This enables the GFM definition.
    pub const GFM_DIALECT: Self = Self(1 << 8);

    pub const GFM: Self = Self(
        Self::TABLES.0
            | Self::TASK_LISTS.0
            | Self::STRIKETHROUGH.0
            | Self::EXTENDED_AUTOLINKS.0
            | Self::TAGFILTER.0
            | Self::GFM_DIALECT.0,
    );
    pub const MATH: Self = Self(Self::MATH_DOLLARS.0 | Self::MATH_CODE.0 | Self::MATH_LATEX.0);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl std::ops::BitOr for Options {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Options {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for Options {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

pub use tagfilter::{filter_disallowed_html, filter_disallowed_html_into};

use crate::block::BufferedLeafEvents;

/// A streaming markdown parser. Text should be fed with [`Parser::feed`] until input exhaustion, where
/// upon you must call [`Parser::finish`].
pub struct Parser {
    block_parser: block::BlockParser,
    /// The byte index corresponding to the start of the new line we're currently reading.
    line_start: usize,
    /// The byte index of the byte we'd last read.
    current_pos: usize,
    finished: bool,
}

impl Parser {
    /// Create a new parser with default options.
    pub fn new() -> Self {
        Self {
            block_parser: block::BlockParser::new(Options::empty()),
            line_start: 0,
            current_pos: 0,
            finished: false,
        }
    }

    /// Create a parser with runtime-selected extensions.
    pub fn with_options(options: Options) -> Self {
        Self {
            block_parser: block::BlockParser::new(options),
            line_start: 0,
            current_pos: 0,
            finished: false,
        }
    }

    /// Feed a chunk of text to the parser and receive `(events, read)`.
    /// - `events` is non-empty IFF a block has been fully parsed.
    /// - `read` is non-zero IFF `events` is non-empty. Subsequent calls must then feed `&s[read..]` for valid parsing.
    pub fn feed_chunk<'a>(&mut self, s: &'a str) -> (VecDeque<Event<'a>>, usize) {
        let mut iter = self.feed(s);
        let events = iter.by_ref().collect();
        let read = iter.consumed();
        (events, read)
    }

    /// When the end of the input stream has been reached, this method must be called to flush any remaining
    /// markdown events and close all open blocks.
    pub fn finish<'a>(&mut self, s: &'a str) -> VecDeque<Event<'a>> {
        self.finish_iter(s).collect()
    }

    /// Convenience: parse a complete markdown string into a `Vec` of events.
    pub fn parse_str(markdown: &str, options: Options) -> Vec<Event<'_>> {
        let mut parser = Self::with_options(options);
        let (events, consumed) = parser.feed_chunk(markdown);
        let mut all: Vec<Event<'_>> = events.into_iter().collect();
        let remaining = if consumed > 0 {
            &markdown[consumed..]
        } else {
            markdown
        };
        all.extend(parser.finish(remaining));
        all
    }

    /// Feed a chunk `s` of text. Returns a lazy iterator of zero or more events. Upon iterator exhaustion
    /// you **must** call [`EventIterator::consumed`] to know how many bytes of the input `s` have been
    /// parsed and **must** no longer be included in future calls to this method. Failure to do so will
    /// lead to invalid parsing (and likely panics).
    pub fn feed<'p, 'a>(&'p mut self, s: &'a str) -> EventIterator<'p, 'a> {
        let pos = self.current_pos;
        EventIterator {
            inner: InnerEventIterator {
                parser: self,
                buf: s.as_bytes(),
                actions: VecDeque::new(),
                active_event_source: None,
                mode: IteratorMode::Feed {
                    pos,
                    output_line_start: 0,
                },
            },
        }
    }

    /// Feed any final text (or an empty str slice) to flush remaining events. Necessary for
    /// closing any remaining blocks.
    pub fn finish_iter<'p, 'a>(&'p mut self, s: &'a str) -> EventIterator<'p, 'a> {
        EventIterator {
            inner: InnerEventIterator {
                parser: self,
                buf: s.as_bytes(),
                actions: VecDeque::new(),
                active_event_source: None,
                mode: IteratorMode::Finish {
                    state: FinishState::Tail,
                    batch_active: false,
                },
            },
        }
    }
}

/// Iterator over the events produced by one chunk fed via [`Parser::feed`].
// It is a wrapper b.c. it improves performance with inlining. Dunno why.
pub struct EventIterator<'p, 'a> {
    inner: InnerEventIterator<'p, 'a>,
}

struct InnerEventIterator<'p, 'a> {
    parser: &'p mut Parser,
    buf: &'a [u8],
    actions: VecDeque<Action<'a>>,
    active_event_source: Option<EventOutputSource<'a>>,
    mode: IteratorMode,
}

enum IteratorMode {
    Feed {
        pos: usize,
        /// Start of the most recent physical line whose parse produced output.
        output_line_start: usize,
    },
    Finish {
        state: FinishState,
        batch_active: bool,
    },
}

enum FinishState {
    Tail,
    CloseBlocks,
    Done,
}

enum EventOutputSource<'a> {
    Buffered(block::BufferedLeafEvents<'a>),
    Inline(inline::InlineCursor),
}

impl<'p, 'a> EventIterator<'p, 'a> {
    /// Returns the number of bytes that were read **AND** parsed. The caller MUST exclude these bytes from future reads for valid parsing.
    /// i.e. `buf[consumed..]`. ALWAYS call this after consuming the iterator!
    pub fn consumed(&self) -> usize {
        let IteratorMode::Feed {
            output_line_start, ..
        } = self.inner.mode
        else {
            // Only returned when we're finishing!
            // Entire input is consumed.
            return self.inner.buf.len();
        };

        if self.inner.parser.block_parser.leaf_is_open() {
            output_line_start
        } else {
            self.inner.parser.line_start
        }
    }
}

impl<'p, 'a> Iterator for EventIterator<'p, 'a> {
    type Item = Event<'a>;

    #[inline(always)]
    fn next(&mut self) -> Option<Event<'a>> {
        self.inner.next()
    }
}

#[inline(always)]
fn check_buffered<'a>(
    source: &mut Option<EventOutputSource<'a>>,
    buffered: Option<BufferedLeafEvents<'a>>,
) -> Option<Event<'a>> {
    let mut events = buffered?;

    // SAFETY: `BufferedLeafEvents` is created IFF there it has parsed a leaf (i.e. it has events to emit).
    let event = unsafe { events.next().unwrap_unchecked() };
    *source = Some(EventOutputSource::Buffered(events));
    Some(event)
}

impl<'p, 'a> Iterator for InnerEventIterator<'p, 'a> {
    type Item = Event<'a>;

    #[inline(always)]
    fn next(&mut self) -> Option<Event<'a>> {
        loop {
            // Return any buffered events.
            if let Some(source) = &mut self.active_event_source {
                let event = match source {
                    EventOutputSource::Buffered(events) => events.next(),
                    EventOutputSource::Inline(cursor) => {
                        self.parser.block_parser.next_inline_event(cursor)
                    }
                };

                if let Some(event) = event {
                    return Some(event);
                } // Buffer exhausted

                self.active_event_source = None;
            }

            // Fetch the next set of buffered events.
            // TODO: not truly lazy yet since inline parsing has already been done in block parser
            if let Some(action) = self.actions.pop_front() {
                match action {
                    Action::Event(event) => return Some(event),
                    Action::InlineParse(root) => {
                        if let IteratorMode::Finish { batch_active, .. } = &mut self.mode {
                            *batch_active = true;
                        }
                        let mut cursor = inline::InlineCursor::new(root);
                        if let Some(event) = self.parser.block_parser.next_inline_event(&mut cursor)
                        {
                            self.active_event_source = Some(EventOutputSource::Inline(cursor));
                            return Some(event);
                        }
                        continue;
                    }
                }
            }

            if let IteratorMode::Finish {
                state,
                batch_active,
            } = &mut self.mode
            {
                if *batch_active {
                    self.parser.block_parser.reset_inline();
                    *batch_active = false;
                }
                let buf = self.buf;
                match state {
                    FinishState::Tail => {
                        *state = FinishState::CloseBlocks;

                        // NOTE: line_start instead of pos is used since we want to ensure all remaining lines are flushed
                        if !self.parser.finished && self.parser.line_start < buf.len() {
                            let pos = self.parser.line_start;

                            // Ensure that we only get the end of the line content (excluding newlines)
                            let mut end = buf.len();
                            if buf[end - 1] == b'\n' {
                                end -= 1;

                                // Handle \r\n
                                if end > pos && buf[end - 1] == b'\r' {
                                    end -= 1;
                                }
                            } else if buf[end - 1] == b'\r' {
                                end -= 1;
                            }

                            let mut buffered = None;
                            self.parser.block_parser.parse_line_for_iter(
                                buf,
                                pos..end,
                                &mut self.actions,
                                &mut buffered,
                            );

                            if let Some(event) =
                                check_buffered(&mut self.active_event_source, buffered)
                            {
                                return Some(event);
                            }
                        }
                        continue;
                    }
                    FinishState::CloseBlocks => {
                        *state = FinishState::Done;
                        self.parser.finished = true;
                        let mut buffered = None;
                        self.parser.block_parser.finish_for_iter(
                            buf,
                            &mut self.actions,
                            &mut buffered,
                        );
                        if let Some(event) = check_buffered(&mut self.active_event_source, buffered)
                        {
                            return Some(event);
                        }
                        continue;
                    }
                    FinishState::Done => return None,
                }
            }

            let IteratorMode::Feed {
                pos,
                output_line_start,
            } = &mut self.mode
            else {
                unreachable!()
            };

            // We've exhausted all buffered content. Therefore, we must continue reading any remaining text
            // in the chunk and buffer more content. First, reset all arenas for reuse.
            self.parser.block_parser.reset_inline();

            // Multiple lines may need to be read before the parser is ready to emit events.
            loop {
                if *pos >= self.buf.len() {
                    return None;
                }

                // Locate the next complete line (terminated by LF/CR/CRLF).
                // NOTE: LLVM automatically removed bounds checking from my testing
                let new_bytes = &self.buf[*pos..];
                let line_end = if new_bytes.len() >= 16 {
                    match memchr::memchr2(b'\n', b'\r', new_bytes) {
                        Some(offset) => *pos + offset,
                        None => self.buf.len(),
                    }
                } else {
                    match new_bytes.iter().position(|&b| b == b'\n' || b == b'\r') {
                        Some(offset) => *pos + offset,
                        None => self.buf.len(),
                    }
                };

                // The new content still does not contain a line ending to terminate the current line.
                // Thus, we must wait for the consumer to provide further data.
                if line_end >= self.buf.len() {
                    *pos = self.buf.len();
                    return None;
                }

                let mut next_line_start = line_end + 1;

                // CommonMark considers `\r`, `\n`, `\r\n` valid line endings. If we blindly accept any `\r`, though, `\r\n`
                // would get decomposed into two separate lines leading to invalid parsing. So, we must check if the
                // next byte is `\n` or await further input.
                if self.buf[line_end] == b'\r' {
                    if line_end + 1 == self.buf.len() {
                        *pos = line_end; // to include `\r` for the next round
                        return None;
                    }

                    // SAFETY: already bounds checked above (LLVM didn't eliminate this on my computer as of commit date)
                    if unsafe { self.buf.get_unchecked(line_end + 1) } == &b'\n' {
                        next_line_start += 1;
                    }
                }

                // Intentionally omits the line endings since they're not useful.
                // TODO: they are useful so we shouldn't omit them...
                let line_start = self.parser.line_start;
                let line = line_start..line_end;
                let mut buffered = None;
                self.parser.block_parser.parse_line_for_iter(
                    self.buf,
                    line,
                    &mut self.actions,
                    &mut buffered,
                );

                self.parser.line_start = next_line_start;
                *pos = next_line_start;

                if let Some(mut events) = buffered {
                    // Created events; drop old text
                    *output_line_start = line_start;
                    self.parser.current_pos = *pos;

                    // SAFETY: `BufferedLeafEvents` is created IFF there it has parsed a leaf (i.e. it has
                    // events to emit).
                    let event = unsafe { events.next().unwrap_unchecked() };
                    self.active_event_source = Some(EventOutputSource::Buffered(events));
                    return Some(event);
                }

                if !self.actions.is_empty() {
                    // Created events; drop old text
                    *output_line_start = line_start;
                    self.parser.current_pos = *pos;

                    // We have inline events to return!
                    break;
                }
            }
        }
    }
}

impl Drop for InnerEventIterator<'_, '_> {
    fn drop(&mut self) {
        if matches!(self.mode, IteratorMode::Feed { .. }) {
            // We can't drop until we consume the entire iterator. The block parser's state has already changed,
            // and so we must also change the user-facing parser!
            while self.next().is_some() {}

            self.parser.block_parser.reset_inline();

            let IteratorMode::Feed {
                pos,
                output_line_start,
            } = self.mode
            else {
                unreachable!()
            };

            // Update the parent parser's position so that it knows what content it needs for future feeds.
            if self.parser.block_parser.leaf_is_open() {
                let consumed = output_line_start;
                self.parser.block_parser.update_leaf_spans(consumed);
                self.parser.line_start -= consumed;
                self.parser.current_pos = pos - consumed;
            } else {
                // We consumed a block and thus we must prep the parser to be ready for the next block!
                let consumed = self.parser.line_start;
                self.parser.line_start = 0;

                // this will always be valid since the consumed bytes must be included in the new block
                // TODO: fix bad comment above...
                self.parser.current_pos = pos - consumed;
            }
        } else {
            self.parser.block_parser.reset_inline();
        }
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}
