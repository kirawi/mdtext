use crate::block::{Span, is_ws};
use crate::utils::{Ld, Location, bytes_has_nul};
use crate::{Event, Options, Tag};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::ops::{Index, IndexMut};

const INLINE_ARENA_CAPACITY: usize = 8;

/// One inline node in the parse tree of a paragraph / heading / cell.
#[derive(Clone, Copy)]
enum InlineData {
    /// Root value and tombstone for detached arena nodes.
    Empty,
    // Indexes into `InlineParser::cowstrs`.
    Text(usize),

    CodeSpan,
    Code(usize),

    Math(usize),
    DisplayMathBlock,
    DisplayMath(usize),

    SoftBreak,
    HardBreak,

    Html(usize),

    Emphasis,
    Strong,
    Strikethrough,

    // Indexes into `InlineParser::links`.
    Link(usize),
    Image(usize),
}

/// Return the end of one physical line, including its line terminator.
fn line_end(bytes: &[u8], start: usize) -> usize {
    let Some(offset) = memchr::memchr2(b'\n', b'\r', &bytes[start..]) else {
        return bytes.len();
    };
    let terminator = start + offset;
    if bytes[terminator] == b'\r' && bytes.get(terminator + 1) == Some(&b'\n') {
        terminator + 2
    } else {
        terminator + 1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeId(NonZeroU32);

impl NodeId {
    fn from_index(index: usize) -> Self {
        debug_assert!(
            index <= u32::MAX as usize,
            "node count exceeds u32 range only in pathological cases"
        );
        // SAFETY: `InlineTree::new()` always reserves index 0 for the root, and this method is only called
        // when creating a new node at an index (i.e. index >=1). Furthermore, it's almost impossible for
        // the maximum limit of 2^32 to be exceeded as that requires a single leaf to exceed **96 GiB** of memory.
        // = 2^32 * 24 (size of Node)
        // Not impossible, but let's be sane here. No real document in its entirety would ever be that large.
        // Assets on parse_inline() would stop any leaf >= 96 GiB as well.
        Self(unsafe { NonZeroU32::new_unchecked(index as u32) })
    }

    fn index(self) -> usize {
        self.0.get() as usize
    }
}

struct Node {
    data: InlineData,
    child: Option<NodeId>,
    next: Option<NodeId>,
}

#[derive(Clone, Copy)]
pub struct InlineRoot {
    current: Option<NodeId>,
    has_nul: bool,
}

pub struct InlineCursor {
    current: Option<NodeId>,
    parent: Option<NodeId>,
    has_nul: bool,
}

impl InlineCursor {
    pub fn new(root: InlineRoot) -> Self {
        Self {
            current: root.current,
            parent: None,
            has_nul: root.has_nul,
        }
    }

    #[inline(always)]
    fn enter(
        &mut self,
        tree: &mut InlineForest,
        current: NodeId,
        child: Option<NodeId>,
        next: Option<NodeId>,
    ) {
        // Because this struct is used only when emitting events (i.e. don't care about prior nodes), we
        // can be clever about the fact that we're allowed to be destructive! This is an optimization where
        // we effectively create a parent relationship on-the-fly since we know that we don't need to go
        // back down to `child` again. *pats self on shoulder*
        tree[current].child = next;
        tree[current].next = self.parent;
        self.parent = Some(current);
        self.current = child;
    }
}

// Idea borrowed from pulldown-cmark
struct InlineForest {
    nodes: Vec<Node>,
    current: Option<NodeId>,
}

impl InlineForest {
    fn new() -> Self {
        let mut nodes = Vec::with_capacity(INLINE_ARENA_CAPACITY);
        nodes.push(Node {
            data: InlineData::Empty,
            child: None,
            next: None,
        });
        Self {
            nodes,
            current: None,
        }
    }

    fn root_child(&self) -> Option<NodeId> {
        self.nodes[0].child
    }

    fn append(&mut self, data: InlineData) -> NodeId {
        let node = self.create_node(data);
        if let Some(current) = self.current {
            self[current].next = Some(node);
        } else {
            self.nodes[0].child = Some(node);
        }
        self.current = Some(node);
        node
    }

    fn create_node(&mut self, data: InlineData) -> NodeId {
        let node = NodeId::from_index(self.nodes.len());
        self.nodes.push(Node {
            data,
            child: None,
            next: None,
        });
        node
    }
}

impl Index<NodeId> for InlineForest {
    type Output = Node;

    fn index(&self, index: NodeId) -> &Self::Output {
        // SAFETY: NodeIds are **never** constructed on-the-fly, and are only ever derived from
        // create_node. Therefore, always in bounds.
        debug_assert!(index.index() < self.nodes.len());
        unsafe { self.nodes.get_unchecked(index.index()) }
    }
}

impl IndexMut<NodeId> for InlineForest {
    fn index_mut(&mut self, index: NodeId) -> &mut Self::Output {
        // SAFETY: see `Index`.
        debug_assert!(index.index() < self.nodes.len());
        unsafe { self.nodes.get_unchecked_mut(index.index()) }
    }
}

pub fn parse_inline<'a, 's>(
    parser: &mut InlineParser<'static, 'static>,
    bytes: &'a [u8],
    spans: &'s [Span],
    options: Options,
) -> InlineRoot {
    /// Due to pointer compression, a chunk exceeding 96 GiB *may* exceed the arena.
    const MAX_FEED_LEN: u64 = 96 * 1024 * 1024 * 1024;

    let ld = Ld::new(bytes, spans);

    let bytes = &ld.buf()[ld.content_start()..ld.len];
    assert!(
        (bytes.len() as u64) < MAX_FEED_LEN,
        "leaf must be smaller than 96 GiB!"
    );

    let has_nul = if ld.buf().len() >= 16 {
        memchr::memchr(b'\0', bytes).is_some()
    } else {
        bytes.contains(&b'\0')
    };

    // SAFETY: It's guaranteed that InlineParser's borrows will never outlive the data.
    // - bytes: lives at least as long as the inline tree is valid (borrows in InlineTree get consumed
    //          or reset with new byte input)
    // - spans: no references to spans persist after `parser.parse()`
    let parser: &mut InlineParser<'a, 's> = unsafe { &mut *(parser as *mut _ as *mut _) };
    parser.begin(ld, options);
    parser.parse();
    InlineRoot {
        current: parser.forest.root_child(),
        has_nul,
    }
}

fn take_cowstr<'s, 'a>(cowstrs: &'s mut [Cow<'a, str>], index: usize) -> Cow<'a, str> {
    debug_assert!(index < cowstrs.len());
    // SAFETY: `index` is always actual index  returned when inserting strings into the arena;
    // it is never arbitrarily constructed.
    std::mem::take(unsafe { cowstrs.get_unchecked_mut(index) })
}

fn take_link<'s, 'a>(links: &'s mut [LinkInfo<'a>], index: usize) -> LinkInfo<'a> {
    debug_assert!(index < links.len());
    // SAFETY: `index` is always actual index returned when inserting linkdata into the arena;
    // it is never arbitrarily constructed.
    std::mem::take(unsafe { links.get_unchecked_mut(index) })
}

const CAN_OPEN: u8 = 1;
const CAN_CLOSE: u8 = 2;

#[derive(Clone, Copy)]
struct DelimInfo {
    delim: u8,
    // CONTEXT: Decreases as delims get consumed for emphasis (but GFM dialect: all consumed so unused)
    count: u16,
    original_count: u16,
    // Bitflags for whether a delimiter can open/close
    flags: u8,
}

impl DelimInfo {
    #[inline]
    fn can_open(&self) -> bool {
        (self.flags & CAN_OPEN) != 0
    }

    #[inline]
    fn can_close(&self) -> bool {
        (self.flags & CAN_CLOSE) != 0
    }
}

#[derive(Default)]
struct LinkInfo<'a> {
    uri: Cow<'a, str>,
    title: Option<Cow<'a, str>>,
}

enum EntityParse {
    Valid {
        decoded: Cow<'static, str>,
        consumed: usize,
    },
    Invalid {
        consumed: usize,
        rescan: bool,
    },
}

pub struct InlineParser<'a, 's> {
    /// A cursor to the underlying text content being parsed.
    ld: Ld<'a, 's>,
    /// Output nodes being built.
    forest: InlineForest,
    /// Byte position one-past the end of the last inserted node.
    /// Tracks where pending unflushed text begins. Updated by `push_node`.
    pending_text_start: usize,

    delim_stack: Vec<(NodeId, DelimInfo)>,

    /// Positions of `[` / `![` entries in `delim_stack`.
    bracket_stack: Vec<usize>,

    /// Arena for string payloads (to reduce the size of `Inline`).
    cowstrs: Vec<Cow<'a, str>>,

    /// Arena for links (to reduce size of Inline enum)
    links: Vec<LinkInfo<'a>>,
    options: Options,

    // Fast paths
    math_closer_exhausted: [bool; 2],
    math_code_closer_exhausted: bool,
    latex_inline_exhausted: bool,
    latex_display_exhausted: bool,

    // NOTE: it seems slightly faster to store a byte position here and convert to Location at runtime;
    // maybe because it reduces allocation size and code span construction is fairly rare?
    code_runs: Option<HashMap<usize, SmallVec<[usize; 4]>>>,
}

impl<'a, 's> InlineParser<'a, 's> {
    pub fn empty() -> InlineParser<'static, 'static> {
        InlineParser::new(Ld::new(&[], &[]), Options::empty())
    }

    pub fn node_count(&self) -> usize {
        self.forest.nodes.len()
    }

    pub fn unescape_table_pipes_from(&mut self, start: usize) {
        for node in &self.forest.nodes[start..] {
            if let InlineData::Code(index) = node.data
                && self.cowstrs[index].contains("\\|")
            {
                self.cowstrs[index] = Cow::Owned(self.cowstrs[index].replace("\\|", "|"));
            }
        }
    }

    fn new(ld: Ld<'a, 's>, options: Options) -> Self {
        let pending_text_start = ld.content_start();
        Self {
            // text,
            ld,
            // pos: 0,
            forest: InlineForest::new(),
            pending_text_start,
            delim_stack: Vec::new(),
            bracket_stack: Vec::new(),
            cowstrs: Vec::with_capacity(INLINE_ARENA_CAPACITY),
            links: Vec::with_capacity(INLINE_ARENA_CAPACITY),
            options,
            math_closer_exhausted: [false; 2],
            math_code_closer_exhausted: false,
            latex_inline_exhausted: false,
            latex_display_exhausted: false,
            code_runs: None,
        }
    }

    fn begin(&mut self, ld: Ld<'a, 's>, options: Options) {
        self.forest.nodes[0].child = None;
        self.forest.current = None;
        self.delim_stack.clear();
        self.bracket_stack.clear();
        self.ld = ld;
        self.options = options;
        self.pending_text_start = self.ld.content_start();
        self.math_closer_exhausted = [false; 2];
        self.math_code_closer_exhausted = false;
        self.latex_inline_exhausted = false;
        self.latex_display_exhausted = false;
        self.code_runs = None;
    }

    /// Drop the current parse while retaining all vector allocations.
    pub fn reset(&mut self) {
        self.forest.nodes.clear();
        self.forest.nodes.push(Node {
            data: InlineData::Empty,
            child: None,
            next: None,
        });
        self.forest.current = None;
        self.delim_stack.clear();
        self.bracket_stack.clear();
        self.cowstrs.clear();
        self.links.clear();
    }

    /// Consume the next event from a detached inline root.
    // NOTE: Inline helps improve performance idk why
    #[inline(always)]
    pub fn next_inline_event(&mut self, cursor: &mut InlineCursor) -> Option<Event<'a>> {
        loop {
            if let Some(current) = cursor.current.take() {
                let (data, child, next) = {
                    let node = &mut self.forest[current];
                    (
                        std::mem::replace(&mut node.data, InlineData::Empty),
                        node.child.take(),
                        node.next.take(),
                    )
                };
                let mut take_text = |index: usize| {
                    let text = take_cowstr(&mut self.cowstrs, index);
                    if cursor.has_nul && bytes_has_nul(text.as_bytes()) {
                        Cow::Owned(text.replace('\0', "\u{FFFD}"))
                    } else {
                        text
                    }
                };
                let event = match data {
                    InlineData::CodeSpan => {
                        cursor.enter(&mut self.forest, current, child, next);
                        Event::Start(Tag::CodeSpan)
                    }
                    InlineData::DisplayMathBlock => {
                        cursor.enter(&mut self.forest, current, child, next);
                        Event::Start(Tag::DisplayMath)
                    }
                    InlineData::Emphasis => {
                        cursor.enter(&mut self.forest, current, child, next);
                        Event::Start(Tag::Emphasis)
                    }
                    InlineData::Strong => {
                        cursor.enter(&mut self.forest, current, child, next);
                        Event::Start(Tag::Strong)
                    }
                    InlineData::Strikethrough => {
                        cursor.enter(&mut self.forest, current, child, next);
                        Event::Start(Tag::Strikethrough)
                    }
                    InlineData::Link(index) => {
                        cursor.enter(&mut self.forest, current, child, next);
                        let LinkInfo { uri: url, title } = take_link(&mut self.links, index);
                        Event::Start(Tag::Link { url, title })
                    }
                    InlineData::Image(index) => {
                        cursor.enter(&mut self.forest, current, child, next);
                        let LinkInfo { uri: url, title } = take_link(&mut self.links, index);
                        Event::Start(Tag::Image { url, title })
                    }
                    InlineData::Empty => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        continue;
                    }
                    InlineData::Text(index) => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        let text = take_text(index);
                        if text.is_empty() {
                            continue;
                        }
                        Event::Text(text)
                    }
                    InlineData::Code(index) => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        Event::Code(take_text(index))
                    }
                    InlineData::Math(index) => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        Event::InlineMath(take_text(index))
                    }
                    InlineData::DisplayMath(index) => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        Event::DisplayMath(take_text(index))
                    }
                    InlineData::SoftBreak => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        Event::SoftBreak
                    }
                    InlineData::HardBreak => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        Event::HardBreak
                    }
                    InlineData::Html(index) => {
                        debug_assert!(child.is_none());
                        cursor.current = next;
                        Event::Html(take_text(index))
                    }
                };
                return Some(event);
            }

            if let Some(parent) = cursor.parent.take() {
                cursor.parent = self.forest[parent].next.take();
                cursor.current = self.forest[parent].child.take();
                return Some(Event::End);
            }

            return None;
        }
    }

    fn parse(&mut self) {
        // Hmm. Interestingly seems lower with AVX512 than AVX2 maybe?
        let snippet = &self.ld.buf()[self.ld.content_start()..self.ld.len];
        let need_check_http =
            self.options.contains(Options::EXTENDED_AUTOLINKS) && contains_http(snippet);
        let need_check_www =
            self.options.contains(Options::EXTENDED_AUTOLINKS) && contains_www(snippet);
        let need_check_ext_email = self.options.contains(Options::EXTENDED_AUTOLINKS)
            && memchr::memchr(b'@', snippet).is_some();
        let need_check_mailto = need_check_ext_email && contains_mailto(snippet);
        let need_check_xmpp = need_check_ext_email && contains_xmpp(snippet);
        let fast_path = !(need_check_http || need_check_www || need_check_ext_email);

        while self.ld.pos < self.ld.len {
            // Fast-path: it skips spans of text that don't have any inline parsing delimiters, i.e.
            // produces one long plain text span.
            if fast_path {
                // REMINDER: a span is always inside a single line. One line may contain separate
                // leafs that get parsed. So: need cap to leaf length (ld.pos) for correctness!
                let bytes = &self.ld.buf()[..self.ld.len];
                let p = next_starter(bytes, self.ld.pos);

                if p > self.ld.pos {
                    // Found delimiter; update pos for parse!
                    self.ld.pos = p;
                }

                if self.ld.pos >= self.ld.len {
                    // Entire paragraph is plain text.
                    break;
                }
            }

            let ch = self.ld.current_unchecked();
            match ch {
                // Inline LaTeX
                b'\\'
                    if self.ld.peek_next() == Some(b'(')
                        && self.options.contains(Options::MATH_LATEX)
                        && !self.latex_inline_exhausted =>
                {
                    self.try_parse_latex_math(false);
                }
                // Display LaTeX
                b'\\'
                    if self.ld.peek_next() == Some(b'[')
                        && self.options.contains(Options::MATH_LATEX)
                        && !self.latex_display_exhausted =>
                {
                    self.try_parse_latex_math(true);
                }

                // Escapes
                b'\\' => self.try_parse_escape(),

                // Code spans
                b'`' => self.try_parse_code_span(),

                // GitHub math syntax.
                b'$' if self.options.contains(Options::MATH_DOLLARS)
                    || self.options.contains(Options::MATH_CODE) =>
                {
                    self.scan_math()
                }

                // Emphasis
                b'*' | b'_' => self.scan_emphasis_run(),
                // Strikethrough
                b'~' if self.options.contains(Options::STRIKETHROUGH) => self.scan_emphasis_run(),

                // Link opener
                b'[' => self.handle_link_open(false),
                // Image link opener
                b'!' if self.ld.peek_next() == Some(b'[') => {
                    self.handle_link_open(true);
                }
                // Link closer
                b']' => self.try_parse_link(),

                // Autolinks/HTML
                b'<' => self.try_parse_autolink_or_html(),

                // Entities
                b'&' => self.try_parse_entity(),

                // Line breaks
                b'\n' | b'\r' => self.parse_line_break(),

                // GFM-extended autolinks (all below)
                b'h' | b'H'
                    if need_check_http
                        && is_extended_autolink_left_boundary(self.ld.text(), self.ld.pos)
                        && self
                            .ld
                            .buf()
                            .get(self.ld.pos..self.ld.pos + 4)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"http")) =>
                {
                    self.try_parse_extended_http_autolink();
                }
                b'w' if need_check_www
                    && is_extended_autolink_left_boundary(self.ld.text(), self.ld.pos)
                    && self.ld.text_here().starts_with("www.") =>
                {
                    self.try_parse_extended_www_autolink();
                }
                b'm' if need_check_mailto
                    && is_extended_autolink_left_boundary(self.ld.text(), self.ld.pos) =>
                {
                    self.try_parse_extended_email_protocol_autolink();
                }
                b'x' if need_check_xmpp
                    && is_extended_autolink_left_boundary(self.ld.text(), self.ld.pos) =>
                {
                    self.try_parse_extended_email_protocol_autolink();
                }
                b'@' if need_check_ext_email => self.try_parse_extended_email_autolink(),

                // Plain Text
                _ => {
                    self.ld.pos += 1;
                }
            }
        }

        // Flush remaining text (must strip trailing whitespace)
        while self.ld.pos > self.pending_text_start
            && matches!(self.ld.get_unchecked(self.ld.pos - 1), b' ' | b'\t')
        {
            self.ld.pos -= 1;
        }

        if self.ld.pos > self.pending_text_start {
            let s = self.ld.borrow(self.pending_text_start..self.ld.pos);
            self.push_cowstr_node(s, InlineData::Text);
        }

        // Resolve any remaining emphasis spans
        if !self.delim_stack.is_empty() {
            self.process_emphasis_with_stack(None);
        }
    }

    /// Create a hashmap of all backtick runs within the string indexed by length. Lazy init.
    /// `{ length, [start] }`
    fn get_code_runs(&mut self) -> &HashMap<usize, SmallVec<[usize; 4]>> {
        /// Index all backtick runs in one physically contiguous content span.
        fn init(bytes: &[u8], base: usize, runs: &mut HashMap<usize, SmallVec<[usize; 4]>>) {
            if bytes.len() < 16 {
                let mut pos = 0;
                while pos < bytes.len() {
                    if bytes[pos] != b'`' {
                        pos += 1;
                        continue;
                    }
                    let start = pos;
                    while pos < bytes.len() && bytes[pos] == b'`' {
                        pos += 1;
                    }
                    runs.entry(pos - start).or_default().push(base + start);
                }
                return;
            }

            let mut ticks = memchr::memchr_iter(b'`', bytes).peekable();
            while let Some(start) = ticks.next() {
                let mut end = start + 1;
                while ticks.peek() == Some(&end) {
                    end += 1;
                    ticks.next();
                }
                runs.entry(end - start).or_default().push(base + start);
            }
        }

        if self.code_runs.is_some() {
            return self.code_runs.as_ref().unwrap();
        }

        let mut runs = HashMap::new();
        let bytes = self.ld.buf();
        if self.ld.is_contiguous() {
            init(&bytes[self.ld.pos..self.ld.len], self.ld.pos, &mut runs);
        } else {
            for span in self.ld.spans_from(self.ld.span_idx) {
                let start = span.start.max(self.ld.pos);
                if start < span.end {
                    init(&bytes[start..span.end], start, &mut runs);
                }
            }
        }
        self.code_runs = Some(runs);
        self.code_runs.as_ref().unwrap()
    }

    fn push_node(&mut self, node: InlineData) -> NodeId {
        let node = self.forest.append(node);
        self.pending_text_start = self.ld.pos;
        node
    }

    fn alloc_cowstr(&mut self, value: Cow<'a, str>) -> usize {
        let index = self.cowstrs.len();
        self.cowstrs.push(value);
        index
    }

    fn push_cowstr_node(
        &mut self,
        value: Cow<'a, str>,
        make_node: fn(usize) -> InlineData,
    ) -> NodeId {
        let index = self.alloc_cowstr(value);
        self.push_node(make_node(index))
    }

    fn push_display_formula(&mut self, source: Cow<'a, str>) {
        let display = self.forest.append(InlineData::DisplayMathBlock);

        match source {
            Cow::Borrowed(source) => {
                if source.is_empty() {
                    let index = self.alloc_cowstr(Cow::Borrowed(""));
                    self.forest.append(InlineData::DisplayMath(index));
                } else {
                    let mut start = 0;
                    while start < source.len() {
                        let end = line_end(source.as_bytes(), start);
                        let index = self.alloc_cowstr(Cow::Borrowed(&source[start..end]));
                        self.forest.append(InlineData::DisplayMath(index));
                        start = end;
                    }
                }
            }
            Cow::Owned(source) => {
                if source.is_empty() {
                    let index = self.alloc_cowstr(Cow::Owned(source));
                    self.forest.append(InlineData::DisplayMath(index));
                } else {
                    let mut start = 0;
                    while start < source.len() {
                        let end = line_end(source.as_bytes(), start);
                        let index = self.alloc_cowstr(Cow::Owned(source[start..end].to_owned()));
                        self.forest.append(InlineData::DisplayMath(index));
                        start = end;
                    }
                }
            }
        }

        self.forest[display].child = self.forest[display].next.take();
        self.forest.current = Some(display);
        self.pending_text_start = self.ld.pos;
    }

    fn node_text(&self, node: NodeId) -> Option<&str> {
        let InlineData::Text(index) = self.forest[node].data else {
            return None;
        };
        Some(&self.cowstrs[index])
    }

    /// Push a link and deactivate prior potential (unmatched) link openers; they are no longer valid.
    fn push_link(&mut self, uri: Cow<'a, str>, title: Option<Cow<'a, str>>, visible: Cow<'a, str>) {
        self.disable_links();
        let index = self.links.len();
        self.links.push(LinkInfo {
            uri: uri.into(),
            title: title.map(Into::into),
        });
        let link = self.push_node(InlineData::Link(index));
        let visible = self.alloc_cowstr(visible);
        let child = self.forest.create_node(InlineData::Text(visible));
        self.forest[link].child = Some(child);
    }

    fn disable_links(&mut self) {
        for &stack_pos in &self.bracket_stack {
            let node = self.delim_stack[stack_pos].0;
            if self.node_text(node) != Some("![") {
                self.delim_stack[stack_pos].1.flags &= !CAN_OPEN;
            }
        }
    }

    fn flush_text(&mut self) {
        if self.ld.pos > self.pending_text_start {
            let s = self.ld.borrow(self.pending_text_start..self.ld.pos);
            if !s.is_empty() {
                self.push_cowstr_node(s, InlineData::Text);
            }
        }
    }

    // -------------------------------------------------------------------------
    // ###                             Scanners                              ###
    // -------------------------------------------------------------------------

    fn try_parse_escape(&mut self) {
        // A backslash at the end of content is literal.
        if self.ld.pos + 1 >= self.ld.len {
            self.ld.pos += 1;
            return;
        }
        let b = self.ld.get_unchecked(self.ld.pos + 1);

        if matches!(b, b'\n' | b'\r') {
            // A backslash DOES NOT create a hard line break at the **end** of a paragraph, i.e. final line!
            if !self.ld.has_next_line() {
                self.ld.pos += 1;
                return;
            }

            self.flush_text();
            assert!(self.ld.advance_line());

            // Leading whitespace on paragraphs must be removed.
            while self.ld.pos < self.ld.len && matches!(self.ld.current_unchecked(), b' ' | b'\t') {
                self.ld.advance(1);
            }

            // This is deferred to here so that pending_text_start gets updated correctly
            self.push_node(InlineData::HardBreak);
        } else if is_ascii_punctuation(b) {
            self.flush_text();
            self.pending_text_start = self.ld.pos + 1; // skip `\`
            self.ld.pos += 2; // skip the `\` and punctuation byte
        } else {
            self.ld.pos += 1; // the `\` is a literal
        }
    }

    // TODO: normalize line endings to spaces!
    fn try_parse_latex_math(&mut self, display: bool) {
        let initial = self.ld.location();
        let mut content_start = initial;
        if !self.ld.advance_location_by(&mut content_start, 2) {
            // consume `\` + `[` / `(`
            return;
        }

        let close_byte = if display { b']' } else { b')' };

        // Look for a closer.
        // NOTE: Not trying optimizing for it like with codespans because am tired. Simple works.
        let mut search = content_start;
        while self.ld.byte_at_location(search).is_some() {
            let bs_pos = match self.ld.find_byte_from_location(b'\\', search) {
                Some(location) => location,
                None => break,
            };

            let mut end = bs_pos;
            if self.ld.advance_location(&mut end)
                && self.ld.byte_at_location(end) == Some(close_byte)
            {
                // Found the closer!
                self.ld.advance_location(&mut end);
                self.flush_text(); // flush pending text up to the `\` opener

                // Create math content
                let source = self.ld.slice(content_start.pos..bs_pos.pos);
                self.ld.seek_location(end);

                if display {
                    self.push_display_formula(source);
                } else {
                    self.push_cowstr_node(source, InlineData::Math);
                }
                return;
            }
            search = bs_pos;
            if !self.ld.advance_location(&mut search) {
                break;
            }
        }

        // No closer was found, so we set this as a fast path
        // TODO: make comment gooder
        if display {
            self.latex_display_exhausted = true;
        } else {
            self.latex_inline_exhausted = true;
        }

        // Reset position since this wasn't LaTeX (i.e. reparse as escaped bracket).
        self.ld.seek_location(initial);
    }

    fn try_parse_code_span(&mut self) {
        let opener_start = self.ld.pos;
        let mut tick_count = 1; // consume `` ` `` from entry
        self.ld.advance(1);

        // Get the run length
        while self.ld.current() == Some(b'`') {
            tick_count += 1;
            self.ld.advance(1);
        }
        let content_start = self.ld.pos;

        // Find a potential code span closer. Its end is implied by the run length.
        let span_closer = self.get_code_runs().get(&tick_count).and_then(|runs| {
            let index = runs.partition_point(|&start| start < content_start);
            runs.get(index).copied()
        });

        let Some(closer_start) = span_closer else {
            // No closer found; treat as literal
            return;
        };
        let closer_end = closer_start + tick_count;

        // Found a code span. So, flush all the pending text prior to the opening backtick.
        if opener_start > self.pending_text_start {
            let text = self.ld.borrow(self.pending_text_start..opener_start);
            self.push_cowstr_node(text, InlineData::Text);
        }

        let close = self.ld.location_at(closer_start);
        let mut trailing = close;
        self.ld.prev_by_location(&mut trailing);

        // Per spec, code spans that begin and end with a space must have a single space removed from both ends
        // IFF it's not entirely composed of whitespace.
        let mut code_end = closer_start;
        let mut start = self.ld.location();
        if matches!(self.ld.get_unchecked(self.ld.pos), b' ' | b'\r' | b'\n')
            && matches!(self.ld.get_unchecked(trailing.pos), b' ' | b'\r' | b'\n')
        {
            while self.ld.pos < closer_start {
                let byte = self.ld.current_unchecked();
                if matches!(byte, b'\r' | b'\n') {
                    if !self.ld.advance_line() {
                        break;
                    }
                    continue;
                }

                if byte != b' ' {
                    break;
                }
                self.ld.pos += 1;
            }

            if self.ld.pos < closer_start {
                // Trim
                self.ld.advance_location(&mut start);
                code_end = trailing.pos;
            }
        }
        self.ld.seek_location(start);

        // Construct the code span node
        let code_span = self.forest.append(InlineData::CodeSpan);
        while self.ld.pos < code_end {
            let b = self.ld.current_unchecked();

            if matches!(b, b'\r' | b'\n') {
                // Newlines must be normalized into spaces
                let code = self.alloc_cowstr(Cow::Borrowed(" "));
                self.forest.append(InlineData::Code(code));

                if b == b'\r'
                    && self.ld.pos + 1 < code_end
                    && self.ld.get_unchecked(self.ld.pos + 1) == b'\n'
                {
                    self.ld.pos += 2;
                } else {
                    self.ld.pos += 1;
                }
                self.ld.advance_line();
            } else {
                let start = self.ld.pos;
                while self.ld.pos < code_end
                    && !matches!(self.ld.current_unchecked(), b'\r' | b'\n')
                {
                    self.ld.pos += 1;
                }

                // SAFETY: stopped at ASCII byte, slice is valid UTF-8.
                let segment =
                    unsafe { std::str::from_utf8_unchecked(&self.ld.buf()[start..self.ld.pos]) };
                let code = self.alloc_cowstr(Cow::Borrowed(segment));
                self.forest.append(InlineData::Code(code));
            }
        }

        // Cursor needs to be placed after closing backticks!
        self.ld.seek_location(Location {
            pos: closer_end,
            span_idx: close.span_idx,
        });

        // Need to ensure all the spans we just pushed are under the codespan event!
        self.forest[code_span].child = self.forest[code_span].next.take();
        self.forest.current = Some(code_span);

        // Need also update to avoid including backtick literals in later text pushes!
        self.pending_text_start = self.ld.pos;
    }

    // TODO: optimize with multiline to avoid alloc on non-contig... but tired
    fn scan_math(&mut self) {
        // Check if it's the backtick delimited form, i.e. $`...`$
        if self.options.contains(Options::MATH_CODE)
            && !self.math_code_closer_exhausted
            && self.ld.peek_next() == Some(b'`')
        {
            // May need to backtrack, hence we use a Location as a cursor
            let mut content_start = self.ld.location();
            self.ld.advance_location_by(&mut content_start, 2);

            // Look for a closer `` `$ ``
            if let Some(closer) = self.ld.find_subslice_from_location(b"`$", content_start) {
                // GitHub's behavior: $``$ gets parsed as regular $ ... $ with `` ` `` as content.
                if closer != content_start {
                    // GitHub does not treat \`$ as a valid closer, and it forbids subsequent matches.
                    if is_escaped_location(&self.ld, closer) {
                        self.ld.advance(1);
                        return;
                    }

                    let mut after_closer = closer;
                    self.ld.advance_location_by(&mut after_closer, 2);
                    self.flush_text();

                    let s = self.ld.slice(content_start.pos..closer.pos);
                    self.ld.seek_location(after_closer);
                    self.push_cowstr_node(s, InlineData::Math);
                    return;
                }
            } else {
                // No closers were found; fast path enabled
                self.math_code_closer_exhausted = true;
            }
        }

        if !self.options.contains(Options::MATH_DOLLARS) {
            self.ld.pos += 1;
            return;
        }

        let mut width = 1;
        while self.ld.get(self.ld.pos + width) == Some(b'$') {
            width += 1;
        }

        // Only $ and $$ are allowed
        if width > 2 {
            // Treat as literal and skip.
            self.ld.pos += width;
            return;
        }

        let mut content_start = self.ld.location();
        self.ld.advance_location_by(&mut content_start, width);

        // There may not be whitespace after opener!
        if self.ld.byte_at_location(content_start).is_none()
            || (width == 1
                && self
                    .ld
                    .char_at(content_start)
                    .is_none_or(is_unicode_whitespace))
        {
            self.ld.seek_location(content_start);
            return;
        }

        // Checks if any closer for the respective $ or $$ exists (fast path)
        let cache_index = width - 1;
        if self.math_closer_exhausted[cache_index] {
            self.ld.seek_location(content_start);
            return;
        }

        // Look for a potential closer.
        if let Some(run_start) = self.ld.find_byte_from_location(b'$', content_start) {
            let mut run_end = run_start;
            while self.ld.byte_at_location(run_end) == Some(b'$') {
                if !self.ld.advance_location(&mut run_end) {
                    break;
                }
            }

            if run_start == content_start {
                self.ld.seek_location(content_start);
                return;
            }

            // Closer rules being applied here.
            if run_end.pos - run_start.pos == width && !is_escaped_location(&self.ld, run_start) {
                let valid = if width == 1 {
                    // May not be preceded by whitespace or followed by a digit (e.g. "5$" or " $5")
                    let before = self.ld.char_before_location(run_start);
                    let after = self.ld.char_at(run_end);
                    !before.is_none_or(is_unicode_whitespace)
                        && !after.is_some_and(|c| c.is_ascii_digit())
                } else {
                    true
                };

                if valid {
                    // Found closer; commit!
                    self.flush_text();

                    let source = self.ld.slice(content_start.pos..run_start.pos);
                    self.ld.seek_location(run_end);

                    if width == 1 {
                        self.push_cowstr_node(source, InlineData::Math);
                    } else {
                        self.push_display_formula(source);
                    }
                    return;
                }
            }
        } else {
            self.math_closer_exhausted[cache_index] = true;
        }

        // Fallthrough: we need to backtrack (math opener delims become literals)
        self.ld.seek_location(content_start);
    }

    // TODO: Could add a Inline::MaybeEmphasis variant to track counts rather than use text... but am lazy
    fn scan_emphasis_run(&mut self) {
        let delim = self.ld.current_unchecked();
        self.ld.advance(1);

        let mut count = 1;
        while self.ld.current() == Some(delim) {
            self.ld.advance(1);
            count += 1;
        }

        // Unicode char required now
        let start = self.ld.pos - count;
        let start_location = Location {
            pos: start,
            span_idx: self.ld.span_idx,
        };
        let end_location = self.ld.location();
        let prev_c = self.ld.char_before_location(start_location);
        let next_c = self.ld.char_at(end_location);

        // Whether it can open/close emphasis
        let flags = classify_delim_run(prev_c, next_c, delim, self.options);

        // No delimiter stack entry needed, so keep it in pending literal text.
        if flags == 0 || delim == b'~' && count >= 3 {
            return;
        }

        let end = self.ld.pos;
        self.ld.pos = start;
        self.flush_text();
        self.ld.pos = end;

        let delim_text = self.ld.borrow(start..self.ld.pos);
        let node = self.push_cowstr_node(delim_text, InlineData::Text);

        // GFM strikethrough: only `~~` is valid, but GitHub supports `~` too. So support both.
        self.delim_stack.push((
            node,
            DelimInfo {
                delim,
                count: count as u16,
                original_count: count as u16,
                flags,
            },
        ));
    }

    fn handle_link_open(&mut self, image: bool) {
        self.flush_text();

        let label = if image { "![" } else { "[" };
        self.ld.advance(label.len()); // consume

        let node = self.push_cowstr_node(label.into(), InlineData::Text);
        self.bracket_stack.push(self.delim_stack.len());
        self.delim_stack.push((
            node,
            DelimInfo {
                delim: b'[',
                count: 1,
                original_count: 1,
                flags: CAN_OPEN,
            },
        ));
    }

    // Per spec, builds link node
    #[inline(never)]
    fn try_parse_link(&mut self) {
        // TODO: maybe defer flush_text to avoid pushing ] as separate?
        // TODO: maybe figure out more optimal link closing strategy idk; experimented a lot though and
        // not easy! not worth?
        self.flush_text();
        self.ld.advance(1);

        let Some(opener_stack_pos) = self.bracket_stack.pop() else {
            // No opener, so treat as literal
            return;
        };

        // Overall: try to parse a valid link
        let opener = self.delim_stack[opener_stack_pos].0;
        let is_image = self.node_text(opener) == Some("![");

        if !self.delim_stack[opener_stack_pos].1.can_open() {
            self.delim_stack.remove(opener_stack_pos);
            return;
        }

        // [...] is valid link text. Now parse URI and title if any.
        let is_link_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r');

        let mut p = self.ld.location();

        // A ( MUST follow
        if self.ld.byte_at_location(p) != Some(b'(') {
            self.delim_stack.remove(opener_stack_pos);
            return;
        }
        self.ld.advance_location(&mut p);

        // Can have leading ws
        let mut line_endings = 0;
        while self.ld.byte_at_location(p).is_some_and(is_link_ws) {
            if matches!(self.ld.byte_at_location(p), Some(b'\r' | b'\n')) {
                line_endings += 1;
                if line_endings > 1 {
                    self.delim_stack.remove(opener_stack_pos);
                    return;
                }
            }
            self.ld.advance_location(&mut p);
        }

        // Try to parse a link destination, if any:
        // - Angular version
        // - Plain version
        let dest_start = p;
        let url = if self.ld.byte_at_location(p) == Some(b'<') {
            self.ld.advance_location(&mut p);
            let content_start = p;
            let mut next_is_escaped = false;
            while let Some(ch) = self.ld.byte_at_location(p) {
                match ch {
                    b'>' if !next_is_escaped => break,
                    b'<' if next_is_escaped => {}

                    // Invalid
                    b'<' | b'\n' | b'\r' => {
                        self.delim_stack.remove(opener_stack_pos);
                        return;
                    }
                    _ => {}
                }

                next_is_escaped = false;

                // Handle escape
                self.ld.advance_location(&mut p);
                if ch == b'\\'
                    && self
                        .ld
                        .byte_at_location(p)
                        .is_some_and(is_ascii_punctuation)
                {
                    next_is_escaped = true;
                }
            }
            if self.ld.byte_at_location(p).is_none() {
                self.delim_stack.remove(opener_stack_pos);
                return;
            }
            let content_end = p;
            self.ld.advance_location(&mut p);
            unescape_cow(self.ld.slice(content_start.pos..content_end.pos))
        } else {
            let mut depth = 0; // For tracking balanced braces
            let mut next_is_escaped = false;
            while let Some(ch) = self.ld.byte_at_location(p) {
                if is_link_ws(ch) {
                    if depth != 0 {
                        self.delim_stack.remove(opener_stack_pos);
                        return;
                    }
                    break;
                }

                if !next_is_escaped && ch == b')' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if !next_is_escaped && ch == b'(' {
                    depth += 1;

                    // NOTE: Completely arbitrary max depth (spec requires >=3) taken from pulldown-cmark
                    if depth > 32 {
                        self.delim_stack.remove(opener_stack_pos);
                        return;
                    }
                } else if ch.is_ascii_control() {
                    self.delim_stack.remove(opener_stack_pos);
                    return;
                }

                next_is_escaped = false;

                self.ld.advance_location(&mut p);

                if ch == b'\\'
                    && self
                        .ld
                        .byte_at_location(p)
                        .is_some_and(is_ascii_punctuation)
                {
                    next_is_escaped = true;
                }
            }

            if self.ld.byte_at_location(p).is_none() {
                self.delim_stack.remove(opener_stack_pos);
                return;
            }

            unescape_cow(self.ld.slice(dest_start.pos..p.pos))
        };

        let mut saw_separator = false;
        line_endings = 0;

        // See if there is a title
        while self.ld.byte_at_location(p).is_some_and(is_link_ws) {
            if matches!(self.ld.byte_at_location(p), Some(b'\r' | b'\n')) {
                line_endings += 1;
                if line_endings > 1 {
                    self.delim_stack.remove(opener_stack_pos);
                    return;
                }
            }
            saw_separator = true;
            self.ld.advance_location(&mut p);
        }

        // Must begin with any of: " ' (
        let mut title = None;
        let b = self.ld.byte_at_location(p);
        if saw_separator && matches!(b, Some(b'"' | b'\'' | b'(')) {
            let closing_delim = match b.unwrap() {
                b'"' => b'"',
                b'\'' => b'\'',
                b'(' => b')',
                _ => unreachable!(),
            };
            self.ld.advance_location(&mut p);

            let title_start = p;
            let mut next_is_escaped = false;
            let mut after_line_ending = false;

            // Now parse title
            while let Some(ch) = self.ld.byte_at_location(p) {
                if !next_is_escaped && ch == closing_delim {
                    break;
                }

                if closing_delim == b')' && !next_is_escaped && ch == b'(' {
                    self.delim_stack.remove(opener_stack_pos);
                    return;
                }

                if ch == b'\r' || ch == b'\n' {
                    if after_line_ending {
                        self.delim_stack.remove(opener_stack_pos);
                        return;
                    }
                    after_line_ending = true;
                } else if ch != b' ' && ch != b'\t' {
                    after_line_ending = false;
                }

                next_is_escaped = false;

                self.ld.advance_location(&mut p);

                if ch == b'\\'
                    && self
                        .ld
                        .byte_at_location(p)
                        .is_some_and(is_ascii_punctuation)
                {
                    next_is_escaped = true;
                }
            }
            if self.ld.byte_at_location(p).is_none() {
                self.delim_stack.remove(opener_stack_pos);
                return;
            }

            // Though we skipped over escapes earlier, we still need to actually process them before committing to output
            title = Some(unescape_cow(self.ld.slice(title_start.pos..p.pos)));
            self.ld.advance_location(&mut p);

            line_endings = 0;
            while self.ld.byte_at_location(p).is_some_and(is_link_ws) {
                if matches!(self.ld.byte_at_location(p), Some(b'\r' | b'\n')) {
                    line_endings += 1;
                    if line_endings > 1 {
                        self.delim_stack.remove(opener_stack_pos);
                        return;
                    }
                }
                self.ld.advance_location(&mut p);
            }
        }

        if self.ld.byte_at_location(p) != Some(b')') {
            self.delim_stack.remove(opener_stack_pos);
            return;
        }
        self.ld.advance_location(&mut p);

        // Now, process emphasis within the link/image content.
        let first_child = self.forest[opener].next;
        self.process_emphasis_with_stack(NonZeroUsize::new(opener_stack_pos + 1));

        self.ld.seek_location(p);
        let index = self.links.len();
        self.links.push(LinkInfo { uri: url, title });
        self.forest[opener].data = if is_image {
            InlineData::Image(index)
        } else {
            InlineData::Link(index)
        };
        self.forest[opener].child = first_child;
        self.forest[opener].next = None;
        self.forest.current = Some(opener);
        self.pending_text_start = self.ld.pos;

        if !is_image {
            self.disable_links();
        }
    }

    fn try_parse_autolink_or_html(&mut self) {
        let start = self.ld.pos;

        if self.try_parse_autolink() {
            return;
        }

        if let Some((tag, end)) = self.try_parse_raw_html() {
            self.flush_text();
            self.ld.seek_location(end);
            self.push_cowstr_node(tag, InlineData::Html);
            return;
        }

        // It's a literal.
        self.ld.seek(start + 1);
    }

    fn try_parse_raw_html(&self) -> Option<(Cow<'a, str>, Location)> {
        let start = self.ld.location();
        let mut p = start;
        if !self.ld.advance_location(&mut p) {
            return None;
        }

        // Can `return None` early since all cases have mutually exclusive prefixes

        // Check for a CDATA section, declaration, or comment
        if self.ld.byte_at_location(p) == Some(b'!') {
            self.ld.advance_location(&mut p); // past '!'

            // CDATA; accepts anything up to closer
            if self.ld.starts_with_at_location(p, b"[CDATA[") {
                self.ld.advance_location_by(&mut p, 7);
                if let Some(mut end) = self.ld.find_subslice_from_location(b"]]>", p) {
                    self.ld.advance_location_by(&mut end, 3);
                    return Some((self.ld.slice(start.pos..end.pos), end));
                }
                return None;
            }

            // Declaration; first char must be ASCII letter
            if self
                .ld
                .byte_at_location(p)
                .is_some_and(|byte| byte.is_ascii_alphabetic())
            {
                if let Some(mut end) = self.ld.find_byte_from_location(b'>', p) {
                    self.ld.advance_location(&mut end);
                    return Some((self.ld.slice(start.pos..end.pos), end));
                }
                return None;
            }

            // Comment
            if self.ld.starts_with_at_location(p, b"--") {
                if let Some(mut end) = self.ld.find_subslice_from_location(b"-->", p) {
                    self.ld.advance_location_by(&mut end, 3);
                    return Some((self.ld.slice(start.pos..end.pos), end));
                }
                return None;
            }

            return None;
        }

        // Processing instruction
        if self.ld.byte_at_location(p) == Some(b'?') {
            self.ld.advance_location(&mut p); // past `?`
            if let Some(mut end) = self.ld.find_subslice_from_location(b"?>", p) {
                self.ld.advance_location_by(&mut end, 2);
                return Some((self.ld.slice(start.pos..end.pos), end));
            }
            return None;
        }

        // Whitespace scanner: spaces/tabs, at most one line ending
        let scan_ws = |ld: &Ld, p: &mut Location| {
            let mut seen_line_ending = false;
            while let Some(b) = ld.byte_at_location(*p) {
                match b {
                    b' ' | b'\t' => {
                        ld.advance_location(p);
                    }
                    b'\n' | b'\r' if !seen_line_ending => {
                        seen_line_ending = true;
                        ld.advance_location(p);
                    }
                    _ => break,
                }
            }
        };

        // Closing Tag
        if self.ld.byte_at_location(p) == Some(b'/') {
            // Must be followed by a tagname. At least one ASCII letter as start
            self.ld.advance_location(&mut p);
            if !self
                .ld
                .byte_at_location(p)
                .is_some_and(|byte| byte.is_ascii_alphabetic())
            {
                return None;
            }

            while self
                .ld
                .byte_at_location(p)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                self.ld.advance_location(&mut p);
            }

            scan_ws(&self.ld, &mut p);
            if self.ld.byte_at_location(p) == Some(b'>') {
                self.ld.advance_location(&mut p);
                return Some((self.ld.slice(start.pos..p.pos), p));
            }
            return None;
        }

        // Open tag
        if !self
            .ld
            .byte_at_location(p)
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        {
            return None;
        }

        // Check for tagname
        while self
            .ld
            .byte_at_location(p)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            self.ld.advance_location(&mut p);
        }

        loop {
            // Handle any trailing whitespace after tagname/previous attribute
            let ws_start = p;
            scan_ws(&self.ld, &mut p);

            if self.ld.byte_at_location(p).is_none() {
                return None;
            }

            // Closed without further attributes? Either > or />
            if self.ld.byte_at_location(p) == Some(b'>') {
                self.ld.advance_location(&mut p);
                return Some((self.ld.slice(start.pos..p.pos), p));
            }

            if self.ld.starts_with_at_location(p, b"/>") {
                self.ld.advance_location_by(&mut p, 2);
                return Some((self.ld.slice(start.pos..p.pos), p));
            }

            // There must be another attribute then, which requires at least some whitespace after tagname
            if p == ws_start {
                return None;
            }

            // Attribute name MUST start with one of these
            if !self
                .ld
                .byte_at_location(p)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':'))
            {
                return None;
            }

            while self.ld.byte_at_location(p).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
            }) {
                self.ld.advance_location(&mut p);
            }

            // Optional attribute value
            let after_name = p;
            scan_ws(&self.ld, &mut p);

            if self.ld.byte_at_location(p) == Some(b'=') {
                self.ld.advance_location(&mut p);
                scan_ws(&self.ld, &mut p);

                if self.ld.byte_at_location(p).is_none() {
                    return None;
                }

                // Unquoted value?
                if !matches!(self.ld.byte_at_location(p), Some(b'"' | b'\'')) {
                    let value_start = p;
                    while self.ld.byte_at_location(p).is_some_and(|byte| {
                        !matches!(
                            byte,
                            b' ' | b'\t' | b'\n' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`'
                        )
                    }) {
                        self.ld.advance_location(&mut p);
                    }

                    // No valid value followed
                    if p == value_start {
                        return None;
                    }
                } else {
                    // Single/double quoted?
                    let quote = self.ld.byte_at_location(p).expect("checked above");
                    self.ld.advance_location(&mut p);

                    while self
                        .ld
                        .byte_at_location(p)
                        .is_some_and(|byte| byte != quote)
                    {
                        self.ld.advance_location(&mut p);
                    }

                    if self.ld.byte_at_location(p).is_none() {
                        return None;
                    }
                    self.ld.advance_location(&mut p);
                }
            } else {
                p = after_name;
            }
        }
    }

    // State machine design
    fn try_parse_autolink(&mut self) -> bool {
        let base = self.ld.pos + 1; // +1 to skip <

        // Try to parse as a URI first. First char needs to be an ASCII letter
        if let Some(first) = self.ld.get(base)
            && first.is_ascii_alphabetic()
        {
            let mut pos = 1;
            let mut scheme_len = 1;
            while base + pos < self.ld.len {
                let ch = self.ld.get_unchecked(base + pos);

                // Scheme terminator
                if ch == b':' {
                    if scheme_len >= 2 {
                        pos += 1;

                        // Now try to parse the rest.
                        while base + pos < self.ld.len {
                            let ch = self.ld.get_unchecked(base + pos);

                            // Valid URI
                            if ch == b'>' {
                                // Get URI content
                                let content = self.ld.borrow(base..base + pos); // +1 to skip <
                                self.flush_text();
                                self.ld.advance(pos + 2); // +1 for <, +1 for >
                                self.push_link(content.clone(), None, content);
                                return true;
                            }

                            // Forbidden in a URI after scheme
                            if ch.is_ascii_control() || ch == b' ' || ch == b'<' {
                                break;
                            }

                            pos += 1;
                        }
                    } else {
                        // Scheme must be 2-32 ASCII chars (bytes) long
                        break;
                    }
                }

                // Forbidden in a URI scheme
                if ch == b'>'
                    || scheme_len >= 32
                    || !(ch.is_ascii_alphanumeric() || matches!(ch, b'+' | b'.' | b'-'))
                {
                    break;
                }

                scheme_len += 1;
                pos += 1;
            }
        }

        // No URI was found. Try to parse email now.

        // Try to parse the local segment of an email. First char cannot be `@`
        if base >= self.ld.len || !is_email_local_valid_byte(self.ld.get_unchecked(base)) {
            return false;
        }
        let mut pos = 1;
        while base + pos < self.ld.len {
            let ch = self.ld.get_unchecked(base + pos);
            if ch == b'@' {
                pos += 1;
                break;
            }

            // Forbidden in an email's local segment
            if ch == b'>' || !is_email_local_valid_byte(ch) {
                return false;
            }
            pos += 1;
        }

        // Handle loop terminating early
        // Characters are required after `@`
        if base + pos == self.ld.len {
            return false;
        }

        // Now check for domain:
        //
        // Spec's given regex can be interpreted as essentially a cycle differentiated only by a final `-` terminal ->
        // non-accepting`. `.` can follow alphanumeric but must then be followed by an alphanumeric char, for which we can
        // reuse the label_len >= 1 condition
        let mut label_len = 0; // Must end in [1, 63]
        let mut last_was_alnum = false; // Accepting is only if last was alnum
        while base + pos < self.ld.len {
            let ch = self.ld.get_unchecked(base + pos);
            if ch == b'>' {
                if last_was_alnum {
                    let content = self.ld.borrow(base..base + pos); // 1 to skip <
                    self.flush_text();
                    self.ld.pos += pos + 2; // +1 for <, +1 for >
                    self.push_link(format!("mailto:{content}").into(), None, content);
                    return true;
                } else {
                    return false;
                }
            }

            // Check if char is [a-zA-Z0-9-], with `.` allowed after an alphanumeric.

            // But first char CANNOT be hyphen
            if label_len == 0 {
                if !ch.is_ascii_alphanumeric() {
                    return false;
                }
                label_len += 1;
            } else if ch == b'.' && last_was_alnum {
                // Reset since we now entered the `*` part of the regex
                label_len = 0;
                last_was_alnum = false; // An alnum char must always follow `.`
            } else if label_len < 63 && ch.is_ascii_alphanumeric() {
                label_len += 1;
                last_was_alnum = true;
            } else if label_len < 63 && ch == b'-' {
                label_len += 1;
                last_was_alnum = false;
            } else {
                // Exceeded 63 char limit
                return false;
            }
            pos += 1;
        }

        false
    }

    fn try_parse_entity(&mut self) {
        let bytes = &self.ld.buf()[self.ld.pos..];
        match try_parse_entity_ref(bytes) {
            EntityParse::Valid { decoded, consumed } => {
                self.flush_text();
                self.ld.advance(consumed);
                self.push_cowstr_node(decoded, InlineData::Text);
            }
            EntityParse::Invalid { consumed, rescan } => {
                if rescan && self.options.contains(Options::EXTENDED_AUTOLINKS) {
                    self.ld.advance(1);
                } else {
                    self.ld.advance(consumed);
                }
            }
        }
    }

    fn brackets_are_active(&self) -> bool {
        self.bracket_stack
            .iter()
            .any(|&stack_pos| self.delim_stack[stack_pos].1.can_open())
    }

    // GFM extended autolink that starts with www.
    fn try_parse_extended_www_autolink(&mut self) {
        let start = self.ld.pos;
        self.ld.pos += 4; // consume `www.`
        if !self.try_parse_domain() {
            return;
        }
        self.try_parse_autolink_path();

        // TODO: maybe ugly start/end swap?
        let end = self.ld.pos;
        let content = self.ld.borrow(start..end);
        self.ld.pos = start; // rollback for flush_text
        self.flush_text();
        self.ld.pos = end;
        self.push_link(format!("http://{content}").into(), None, content);
    }

    // GFM extended autolink that starts with http:// or https://
    fn try_parse_extended_http_autolink(&mut self) {
        let start = self.ld.pos;
        self.ld.pos += 4; // consume http

        // http(s)
        if self
            .ld
            .current()
            .is_some_and(|b| b.eq_ignore_ascii_case(&b's'))
        {
            self.ld.pos += 1;
        }

        if !self.ld.text()[self.ld.pos..].starts_with("://") {
            return;
        }
        self.ld.pos += 3; // consume `://`

        if !self.try_parse_domain() {
            return;
        }
        self.try_parse_autolink_path();

        // TODO: maybe ugly start/end swap?
        let end = self.ld.pos;
        let content = self.ld.borrow(start..end);
        self.ld.pos = start; // rollback for flush_text
        self.flush_text();
        self.ld.pos = end;
        self.push_link(content.clone(), None, content);
    }

    // NOTE: cmark-gfm escapes `hello@there\.com`. I'll assume this is a bug.
    fn try_parse_extended_email_autolink(&mut self) {
        // FIXME: (see comments for other brackets_are_active call)
        if self.brackets_are_active() {
            self.ld.pos += 1;
            return;
        }

        // Backtracking is unavoidable here!
        let mut start = self.ld.pos - 1; // one before @ symbol

        // Must not interfere with text we've already emitted events for
        while start >= self.pending_text_start {
            let b = self.ld.get_unchecked(start);
            if !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')) {
                start += 1; // don't overextend (needed for flush_text)
                break;
            }
            if start == self.pending_text_start {
                break;
            }
            start -= 1;
        }

        // Needs at least one match
        if start == self.ld.pos {
            self.ld.pos += 1; // treat @ as literal
            return;
        }

        let mut end = self.ld.pos + 1;
        let mut saw_period = false;
        let mut trailing_periods = 0;
        while let Some(b) = self.ld.get(end) {
            if b == b'.' {
                trailing_periods += 1;
            } else if !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
                break;
            } else {
                saw_period |= trailing_periods > 0;
                trailing_periods = 0;
            }
            end += 1;
        }
        end -= trailing_periods;

        // Need at least one non-trailing period. `_` and `-` are allowed inside, but NOT at end.
        if !saw_period || matches!(self.ld.get(end - 1), Some(b'_') | Some(b'-')) {
            // Prohibited per spec
            self.ld.pos += 1; // treat @ as literal
            return;
        }

        let content = self.ld.borrow(start..end);
        self.ld.pos = start;
        self.flush_text();
        self.ld.pos = end;
        self.push_link(format!("mailto:{content}").into(), None, content);
    }

    fn try_parse_extended_email_protocol_autolink(&mut self) {
        // TODO: this is overly restrictive! need final step at end parse where check if in resolved link
        // OR maybe in scan_link_close auto resolve prev such autolinks?
        if self.brackets_are_active() {
            self.ld.pos += 1;
            return;
        }

        let mut text = &self.ld.text()[self.ld.pos..];
        let is_xmpp;
        if text.starts_with("mailto:") {
            text = &text[..7];
            is_xmpp = false;
        } else if text.starts_with("xmpp:") {
            text = &text[..5];
            is_xmpp = true;
        } else {
            self.ld.pos += 1;
            return;
        }

        // The GFM specification is ambiguous, cmark-gfm does NOT use the CommonMark definition of email. Reuses
        // same definition as extended email autolink.
        let mut end = self.ld.pos + text.len();
        while let Some(b) = self.ld.get(end) {
            if !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+')) {
                break;
            }
            end += 1;
        }

        if self.ld.get(end) != Some(b'@') {
            self.ld.pos += text.len();
            return;
        }
        end += 1;

        let mut saw_period = false;
        let mut trailing_periods = 0;
        while let Some(b) = self.ld.get(end) {
            if b == b'.' {
                trailing_periods += 1;
            } else if !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
                break;
            } else {
                saw_period |= trailing_periods > 0;
                trailing_periods = 0;
            }
            end += 1;
        }
        end -= trailing_periods;

        // Need at least one non-trailing period. `_` and `-` are allowed inside, but NOT at end.
        if !saw_period || matches!(self.ld.get(end - 1), Some(b'_') | Some(b'-')) {
            // Prohibited per spec
            self.ld.pos += text.len();
            return;
        }

        // XMPP allows an optional `/resource` after the address.
        if is_xmpp && self.ld.get(end) == Some(b'/') {
            let mut resource_end = end + 1;
            while let Some(b) = self.ld.get(resource_end) {
                if b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.') {
                    resource_end += 1;
                } else {
                    break;
                }
            }
            if resource_end > end + 1 {
                end = resource_end;
            }
        }

        let content = self.ld.borrow(self.ld.pos..end);
        self.flush_text();
        self.ld.pos = end; // to consume email
        self.push_link(content.clone(), None, content);
    }

    /// Returns `false` if it's not a valid domain.
    fn try_parse_domain(&mut self) -> bool {
        // Per segment, set least significant bit to 1 if see underscore. If see period (new segment), left
        // shit by 1. Final check mask 0x3 and see if >0.
        // Totally clever and maybe less readable trick! But why not!
        let mut saw_underscores: u8 = 0;
        let mut saw_period = false;
        let mut valid_end = None;

        // We **must** iterate over chars here to support non-English domains (e.g. こんにちは.com)
        // It is safe to mutate `ld` because no valid domain character is an inline construct starter!
        while let Some(c) = self.ld.current_char() {
            if c == '.' {
                saw_period = true;
                saw_underscores = saw_underscores << 1;
                self.ld.pos += 1;
            } else if c == '_' {
                saw_underscores |= 1;
                self.ld.pos += 1;
            } else if c == '-' || c.is_alphanumeric() {
                self.ld.pos += c.len_utf8();
            } else {
                break;
            }

            // Save before any trailing path punctuation gets consumed as domain.
            if c != '.' && saw_underscores & 0x3 == 0 && saw_period {
                valid_end = Some(self.ld.pos);
            }
        }

        if let Some(end) = valid_end {
            self.ld.pos = end;
            true
        } else {
            false
        }
    }

    fn try_parse_autolink_path(&mut self) {
        // Domain may be followed by path
        // We need to ensure that all parentheses that appear are balanced per spec
        let mut paren_pair: isize = 0;

        // NOTE: ' and " are NOT in GFM-spec, but this is what cmark-gfm does and the behavior makes sense.
        let is_punc = |b: u8| {
            matches!(
                b,
                b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' | b'\'' | b'"'
            )
        };

        let mut search = self.ld.pos;
        while let Some(b) = self.ld.get(search) {
            if b == b'<' || b == b'\n' || b == b'\r' || b.is_ascii_whitespace() {
                break;
            }

            match b {
                b'(' => paren_pair += 1,
                b')' => paren_pair -= 1,
                _ => {}
            }
            search += 1;
        }

        // Trim any trailing punctuation, unmatched closing parentheses, and entity-like trailing semicolons.
        let mut end = search;
        while end > self.ld.pos {
            let b = self.ld.get_unchecked(end - 1);
            if b == b')' && paren_pair < 0 {
                end -= 1;
                paren_pair += 1;
            } else if b == b';' {
                // Check if this section is an entity. NOTE: GFM specification says to check for alphanumeric,
                // but cmark-gfm only checks alpha which is what is done here.
                let mut entity = end - 1;
                while entity > self.ld.pos
                    && self.ld.get_unchecked(entity - 1).is_ascii_alphabetic()
                {
                    entity -= 1;
                }

                if entity < end - 1
                    && entity > self.ld.pos
                    && self.ld.get_unchecked(entity - 1) == b'&'
                {
                    end = entity - 1;
                } else {
                    end -= 1;
                }
            } else if is_punc(b) {
                end -= 1;
            } else {
                break;
            }
        }
        self.ld.pos = end;
    }

    fn parse_line_break(&mut self) {
        // Check if this is a hard line break
        let is_hard = self.ld.pos >= self.pending_text_start + 2
            && self.ld.get_unchecked(self.ld.pos - 1) == b' '
            && self.ld.get_unchecked(self.ld.pos - 2) == b' ';

        let mut end = self.ld.pos;
        if is_hard {
            while end > self.pending_text_start && self.ld.get_unchecked(end - 1) == b' ' {
                end -= 1;
            }
        } else {
            while end > self.pending_text_start && is_ws(self.ld.get_unchecked(end - 1)) {
                end -= 1;
            }
        }
        self.ld.pos = end;

        self.flush_text();
        if !self.ld.advance_line() {
            // End of paragraph
            // IMPORTANT: the final EOL must NEVER be pushed as a line break event in this case!
            self.pending_text_start = self.ld.pos;
            return;
        }

        // Skip also any leading whitespace on the next line.
        let mut start = self.ld.pos;
        while start < self.ld.len && is_ws(self.ld.get_unchecked(start)) {
            start += 1;
        }
        self.ld.pos = start;

        // Deferred to correctly update pending_text_start
        self.push_node(if is_hard {
            InlineData::HardBreak
        } else {
            InlineData::SoftBreak
        });
    }

    /// Run the delimiter-stack algorithm for the selected base dialect.
    fn process_emphasis_with_stack(&mut self, bottom: Option<NonZeroUsize>) {
        let mut stack = if let Some(n) = bottom {
            self.delim_stack.split_off(n.get())
        } else {
            // This is triggered only at the end of parsing when nothing else gets added to the delim stack
            std::mem::take(&mut self.delim_stack)
        };

        // * (6: % 3 × is_open) _ (6: % 3 × is_open) ~ (2: ~ or ~~)
        // Used for tracking the lower bound per delimiter class (ignoring everything prior for just that class)
        let mut openers_bottom = [0_usize; 14];

        // Find a potential closer further up the stack.
        let mut i = 0;
        while i < stack.len() {
            // Skip non-closing delimiters (implicitly excludes `[`)
            if !stack[i].1.can_close() {
                i += 1;
                continue;
            } // So, this is a closing delimiter for an emphasis run.

            let cinfo = &stack[i].1;

            // Now, go back down the stack to find a pairing opener.
            // Requires: same delimiter and run length
            let mut found = false;
            let mut opener_idx_in_stack = 0;
            let info = &stack[i].1;
            let class = match info.delim {
                b'*' => usize::from(info.original_count % 3) + usize::from(info.can_open()) * 3,
                b'_' => 6 + usize::from(info.original_count % 3) + usize::from(info.can_open()) * 3,
                b'~' => 12 + usize::from(info.original_count >= 2),
                _ => 0,
            };
            let bottom = openers_bottom[class].min(i);
            if i > 0 {
                // Now, we need to look backwards (with opener_bottom being the lower bound) for an opener.
                for k in (bottom..i).rev() {
                    let (_, oinfo) = &stack[k];

                    if oinfo.delim == cinfo.delim
                        && oinfo.can_open()
                        && (oinfo.delim != b'~' || oinfo.count == cinfo.count)
                    {
                        // Check for rule of 3 to avoid ambiguous parsing of * and _
                        let rule = (!cinfo.can_open() && !oinfo.can_close())
                            || cinfo.original_count.is_multiple_of(3)
                            || !((oinfo.original_count as usize + cinfo.original_count as usize)
                                .is_multiple_of(3));
                        if rule {
                            found = true;
                            opener_idx_in_stack = k;
                            break;
                        }
                    }
                }
            }

            if !found {
                // Update lower bound: there are no openers below and including this point
                openers_bottom[class] = i;
                i += 1;
                continue;
            }

            // Added to satisfy lifetimes (since stack gets modified later)
            let closer_delim = stack[i].1.delim;

            // Since we found a match, we must remove all delimiters between opener and closer from the stack.
            // Thus, we must clamp the lower bounds to account for what is getting removed.
            for o in openers_bottom.iter_mut() {
                *o = (*o).min(opener_idx_in_stack);
            }

            let opener = stack[opener_idx_in_stack].0;
            let closer = stack[i].0;
            let opener_count = stack[opener_idx_in_stack].1.count;
            let closer_count = stack[i].1.count;

            // IMPORTANT: CommonMark and GitHub-Flavored Markdown diverge in how nested emphasis spans get
            // minimized (i.e. reduce the number of generated elements). This is not explicitly documented
            // by GFM and can only be understood by comparing the examples under their respective
            // "Emphasis and strong emphasis" sections.
            //
            // # GFM Emphasis Minimization:
            // consume `w` = min(opener_count, closer_count) delimiters from both runs
            // - If `w` is even: parse *single* bold
            // - If `w` is odd: parse *single* italic
            //
            // # CommonMark Emphasis Minimization:
            // Iteratively (until either opened_count or closer_count are 0):
            // - If can consume 2: parse bold
            // - If can consume 1: parse italic
            //
            // # Example:
            // ****foo****
            //   - GFM: <strong>foo</strong>
            //   - CommonMark: <strong><strong>foo</strong></strong>
            let used = if closer_delim == b'~' {
                // GFM spec: always a strikethrough (consume all)
                opener_count
            } else if self.options.contains(Options::GFM_DIALECT) {
                opener_count.min(closer_count)
            } else if opener_count >= 2 && closer_count >= 2 {
                // Bold (strong)
                2
            } else {
                // Italics
                1
            };

            // Consume the delimiters for the created emphasis.
            let opener_remaining = opener_count - used;
            let closer_remaining = closer_count - used;
            let keep_opener = opener_remaining != 0;
            let keep_closer = closer_remaining != 0;

            if keep_opener {
                stack[opener_idx_in_stack].1.count = opener_remaining;
            }
            if keep_closer {
                stack[i].1.count = closer_remaining;
            }

            // Remove delimiters between the matching delimiter runs (plus the runs themselves if exhausted).
            let stack_drain_start = opener_idx_in_stack + usize::from(keep_opener);
            let stack_drain_end = i + usize::from(!keep_closer);
            stack.drain(stack_drain_start..stack_drain_end);
            i = opener_idx_in_stack;

            let used = used as usize;
            let first_child = self.forest[opener].next.unwrap();
            let after = self.forest[closer].next;
            let mut last_child = first_child;

            // Find the node that will become the last child and unset its `next` link so the later reparented
            // child linkedlist is properly terminated (otherwise traversing children would eroneously cross to siblings
            // of emphasis container itself).
            while self.forest[last_child].next != Some(closer) {
                last_child = self.forest[last_child].next.unwrap();
            }
            self.forest[last_child].next = None;

            if keep_opener {
                let InlineData::Text(index) = self.forest[opener].data else {
                    unreachable!()
                };
                let text = &mut self.cowstrs[index];
                let n = opener_remaining as usize;
                match text {
                    Cow::Borrowed(b) => *b = &b[..n],
                    Cow::Owned(s) => s.truncate(n),
                }
            }
            if keep_closer {
                let InlineData::Text(index) = self.forest[closer].data else {
                    unreachable!()
                };
                let text = &mut self.cowstrs[index];
                match text {
                    Cow::Borrowed(b) => *b = &b[used..],
                    Cow::Owned(s) => {
                        s.drain(..used);
                    }
                }
            }

            let container = if closer_delim == b'~' {
                InlineData::Strikethrough
            } else if used % 2 == 0 {
                InlineData::Strong
            } else {
                InlineData::Emphasis
            };

            let first_child = if self.options.contains(Options::GFM_DIALECT)
                && matches!(container, InlineData::Strong)
            {
                self.flatten_strong_children(first_child)
            } else {
                first_child
            };
            let container_child =
                if self.options.contains(Options::GFM_DIALECT) && used >= 3 && used % 2 == 1 {
                    let strong = self.forest.create_node(InlineData::Strong);
                    self.forest[strong].child = Some(self.flatten_strong_children(first_child));
                    strong
                } else {
                    first_child
                };

            // Update the tree to reflect the fact that the span of all content within the emphasis are now
            // children of that emphasis.
            //
            // For optimization, try to reuse existing emphasis node if possible
            match (keep_opener, keep_closer) {
                (false, false) => {
                    self.forest[opener].data = container;
                    self.forest[opener].child = Some(container_child);
                    self.forest[opener].next = after;
                    self.forest[closer].data = InlineData::Empty;
                }
                (false, true) => {
                    self.forest[opener].data = container;
                    self.forest[opener].child = Some(container_child);
                    self.forest[opener].next = Some(closer);
                }
                (true, false) => {
                    self.forest[closer].data = container;
                    self.forest[closer].child = Some(container_child);
                    self.forest[closer].next = after;
                    self.forest[opener].next = Some(closer);
                }
                (true, true) => {
                    let wrapper = self.forest.create_node(container);
                    self.forest[wrapper].child = Some(container_child);
                    self.forest[wrapper].next = Some(closer);
                    self.forest[opener].next = Some(wrapper);
                }
            };
        }
    }

    fn flatten_strong_children(&mut self, mut first: NodeId) -> NodeId {
        let mut previous = None;
        let mut current = Some(first);
        while let Some(node) = current {
            let next = self.forest[node].next;
            if matches!(self.forest[node].data, InlineData::Strong) {
                let child = self.forest[node].child.take().expect("strong content");
                let mut last = child;
                while let Some(sibling) = self.forest[last].next {
                    last = sibling;
                }
                self.forest[last].next = next;
                if let Some(previous) = previous {
                    self.forest[previous].next = Some(child);
                } else {
                    first = child;
                }
                self.forest[node].data = InlineData::Empty;
                current = next;
            } else {
                previous = Some(node);
                current = next;
            }
        }
        first
    }
}

fn try_parse_entity_ref(bytes: &[u8]) -> EntityParse {
    use crate::generated_entities::ENTITIES;

    let mut start = 1; // Experiment if cursor faster than whole self.ld.pos
    if bytes.get(start).copied() == Some(b'#') {
        // Hex/Decimal
        start += 1;

        let is_hex = match bytes.get(start) {
            // Hex codepoint
            Some(b) if b == &b'x' || b == &b'X' => {
                start += 1; // skip x X
                true
            }

            // Decimal codepoint
            Some(b) if b.is_ascii_digit() => false,
            _ => {
                return EntityParse::Invalid {
                    consumed: 1,
                    rescan: false,
                };
            }
        };

        let mut end = start;
        let mut value: u32 = 0; // 32 bits is sufficient for 7 digits
        let max_digits = if is_hex { 6 } else { 7 };
        while let Some(b) = bytes.get(end)
            && end < start + max_digits
        {
            if is_hex {
                let digit = match b {
                    b'0'..=b'9' => (b - b'0') as u32,
                    b'a'..=b'f' => (b - b'a' + 10) as u32,
                    b'A'..=b'F' => (b - b'A' + 10) as u32,
                    _ => break,
                };
                value = value * 16 + digit; // remember: hex so each digit is 16!
            } else if b.is_ascii_digit() {
                value = value * 10 + (b - b'0') as u32
            } else {
                break;
            }

            end += 1;
        }

        // Need check ACTUALLY found digit
        if start == end || bytes.get(end) != Some(&b';') {
            // invalid; consume all as literal
            return EntityParse::Invalid {
                consumed: end,
                rescan: false,
            };
        }

        let ch = if value == 0x0000 {
            '\u{FFFD}'
        } else {
            char::from_u32(value).unwrap_or('\u{FFFD}')
        };

        // need to skip over ;
        return EntityParse::Valid {
            decoded: ch.to_string().into(),
            consumed: end + 1,
        };
    } else {
        // Named entities
        let mut end = start;
        while let Some(b) = bytes.get(end)
            && end - start < crate::generated_entities::MAX_ENTITY_NAME_LEN
        {
            // Spec doesn't say, but entities.json only has alphanumeric keys
            if b == &b';' || !b.is_ascii_alphanumeric() {
                break;
            }
            end += 1;
        }

        if bytes.get(end).copied() != Some(b';') {
            // invalid; consume all as literal
            return EntityParse::Invalid {
                consumed: end,
                rescan: false,
            };
        }

        // Try to get a named entity
        let name = &bytes[start..end];
        if let Some(e) = ENTITIES
            .binary_search_by_key(&name, |&(key, _)| key)
            .ok()
            .map(|i| ENTITIES[i].1)
        {
            return EntityParse::Valid {
                decoded: e.into(),
                consumed: end + 1,
            };
        } else {
            // Not a valid entity. Consume & as literal. Need rescan all else for other inline.
            // In practice: only matter if extended autoline since other inline parse depend on punctuation
            return EntityParse::Invalid {
                consumed: end + 1,
                rescan: true,
            };
        }
    }
}

// perf: For some reason, `u8::is_ascii_punctuation` was being compiled into SIMD for single-byte lookups
// for `scan_link_close`. I don't understand why this fixes it even though the code is nearly identical
// to source std... unless it's due to Rust not being able to inline the std lib as well?
const fn is_ascii_punctuation(b: u8) -> bool {
    matches!(
        b,
        b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~'
    )
}

fn is_unicode_punctuation(ch: char, options: Options) -> bool {
    // Fast path: ASCII covers the vast majority of markdown content.
    if ch.is_ascii() {
        return is_ascii_punctuation(ch as u8);
    }
    use unicode_general_category::{GeneralCategory, get_general_category};

    let category = get_general_category(ch);
    if matches!(
        category,
        GeneralCategory::OtherPunctuation
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
    ) {
        return true;
    }

    !options.contains(Options::GFM_DIALECT)
        && matches!(
            category,
            GeneralCategory::MathSymbol
                | GeneralCategory::CurrencySymbol
                | GeneralCategory::ModifierSymbol
                | GeneralCategory::OtherSymbol
        )
}

// Per official unicode specification
fn is_unicode_whitespace(ch: char) -> bool {
    matches!(
        ch,
        // Cc: Tab
        '\u{0009}'
        // Cc: Line feed
        | '\u{000A}'
        // Cc: Form feed
        | '\u{000C}'
        // Cc: Carriage return
        | '\u{000D}'
        // Zs: SPACE
        | '\u{0020}'
        // Zs: NO-BREAK SPACE
        | '\u{00A0}'
        // Zs: OGHAM SPACE MARK
        | '\u{1680}'
        // Zs: EN QUAD .. HAIR SPACE
        | '\u{2000}'
            ..='\u{200A}'
        // Zs: NARROW NO-BREAK SPACE
        | '\u{202F}'
        // Zs: MEDIUM MATHEMATICAL SPACE
        | '\u{205F}'
        // Zs: IDEOGRAPHIC SPACE
        | '\u{3000}'
    )
}

fn is_escaped_location(ld: &Ld, mut location: Location) -> bool {
    let mut backslashes = 0;
    while ld.prev_by_location(&mut location) && ld.byte_at_location(location) == Some(b'\\') {
        backslashes += 1;
    }
    backslashes % 2 == 1
}

#[inline]
fn next_starter_scalar(bytes: &[u8], mut pos: usize) -> usize {
    fn is_inline_starter(byte: u8) -> bool {
        const LOW: u64 = (1 << b'\n')
            | (1 << b'\r')
            | (1 << b'!')
            | (1 << b'$')
            | (1 << b'&')
            | (1 << b'*')
            | (1 << b'<');
        const HIGH: u64 = (1 << (b'[' - 64))
            | (1 << (b'\\' - 64))
            | (1 << (b']' - 64))
            | (1 << (b'_' - 64))
            | (1 << (b'`' - 64))
            | (1 << (b'~' - 64));
        if byte < 64 {
            LOW & (1 << byte) != 0
        } else if byte < 128 {
            HIGH & (1 << (byte - 64)) != 0
        } else {
            false
        }
    }

    while pos < bytes.len() && !is_inline_starter(bytes[pos]) {
        pos += 1;
    }
    pos
}

#[inline]
fn next_starter(bytes: &[u8], pos: usize) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        simd::next_starter_impl(bytes, pos)
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        simd::next_starter_impl(bytes, pos)
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        next_starter_scalar(bytes, pos)
    }
}

/// Per the GFM specification, an autolink may follow **only** any whitespace character
/// or the bytes `*, _, ~, (` without exception.
fn is_extended_autolink_left_boundary(text: &str, start: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || matches!(c, '*' | '_' | '~' | '('))
}

/// Returns the `CAN_OPEN` / `CAN_CLOSE` flags for a delimiter run.
fn classify_delim_run(prev: Option<char>, next: Option<char>, delim: u8, options: Options) -> u8 {
    let prev_is_ws = prev.is_none_or(is_unicode_whitespace);
    let next_is_ws = next.is_none_or(is_unicode_whitespace);

    // Per specification rules for left/right-flank
    let is_left_flanking = !next_is_ws
        && (!next.is_some_and(|ch| is_unicode_punctuation(ch, options))
            || prev_is_ws
            || prev.is_some_and(|ch| is_unicode_punctuation(ch, options)));
    let is_right_flanking = !prev_is_ws
        && (!prev.is_some_and(|ch| is_unicode_punctuation(ch, options))
            || next_is_ws
            || next.is_some_and(|ch| is_unicode_punctuation(ch, options)));

    // * ~ * have different rules for if they can open/close emphasis per spec
    if delim == b'*' || delim == b'~' {
        u8::from(is_left_flanking) * CAN_OPEN | u8::from(is_right_flanking) * CAN_CLOSE
    } else {
        let prev_is_punct = prev.is_some_and(|ch| is_unicode_punctuation(ch, options));
        let next_is_punct = next.is_some_and(|ch| is_unicode_punctuation(ch, options));
        let can_open = is_left_flanking && (!is_right_flanking || prev_is_punct);
        let can_close = is_right_flanking && (!is_left_flanking || next_is_punct);
        u8::from(can_open) * CAN_OPEN | u8::from(can_close) * CAN_CLOSE
    }
}

/// Unescapes any backslash-escaped characters, or any entities.
pub fn unescape_string(s: &str) -> Cow<'_, str> {
    let should_escape = if s.len() >= 16 {
        memchr::memchr2(b'&', b'\\', s.as_bytes()).is_none()
    } else {
        !s.contains('&') && !s.contains('\\')
    };

    if should_escape {
        return Cow::Borrowed(s);
    }

    let mut result = String::with_capacity(s.len());
    let mut pos = 0;
    while pos < s.len() {
        if s.as_bytes()[pos] == b'&' {
            match try_parse_entity_ref(&s.as_bytes()[pos..]) {
                EntityParse::Valid { decoded, consumed } => {
                    result.push_str(&decoded);
                    pos += consumed;
                    continue;
                }
                EntityParse::Invalid { consumed, .. } => {
                    result.push_str(&s[pos..pos + consumed]);
                    pos += consumed;
                    continue;
                }
            }
        }
        if s.as_bytes()[pos] == b'\\'
            && s.as_bytes()
                .get(pos + 1)
                .is_some_and(|b| is_ascii_punctuation(*b))
        {
            result.push(s.as_bytes()[pos + 1] as char);
            pos += 2;
            continue;
        }

        // There is always a subsequent char? maybe
        let ch = s[pos..].chars().next().unwrap();
        result.push(ch);
        pos += ch.len_utf8();
    }
    result.into()
}

/// Unescape a potentially owned slice while preserving a source borrow when
/// no replacement is needed.
fn unescape_cow<'a>(s: Cow<'a, str>) -> Cow<'a, str> {
    match unescape_string(&s) {
        Cow::Borrowed(_) => s,
        Cow::Owned(s) => Cow::Owned(s),
    }
}

fn is_email_local_valid_byte(ch: u8) -> bool {
    // A match creates a dispatch table, so this is slightly faster.
    // A total micro optimization but seems fun
    const LOW: u64 = (1 << b'!')
        | (1 << b'#')
        | (1 << b'$')
        | (1 << b'%')
        | (1 << b'&')
        | (1 << b'\'')
        | (1 << b'*')
        | (1 << b'+')
        | (1 << b'-')
        | (1 << b'.')
        | (1 << b'/')
        | (1 << b'=')
        | (1 << b'?');
    const HIGH: u64 = (1 << (b'^' - 64))
        | (1 << (b'_' - 64))
        | (1 << (b'`' - 64))
        | (1 << (b'{' - 64))
        | (1 << (b'|' - 64))
        | (1 << (b'}' - 64))
        | (1 << (b'~' - 64));

    if ch.is_ascii_alphanumeric() {
        true
    } else if ch < 64 {
        LOW & (1 << ch) != 0
    } else if ch < 128 {
        HIGH & (1 << (ch - 64)) != 0
    } else {
        false
    }
}

fn contains_http(bytes: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        simd::contains_http_impl(bytes)
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        simd::contains_http_impl(bytes)
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        contains_http_scalar(bytes)
    }
}

// A mask for case-insensitive HTTP.
const HTTP_MASK_SCALAR: u32 =
    u32::from_le_bytes([b'h' | 0x20, b't' | 0x20, b't' | 0x20, b'p' | 0x20]);

/// Check for case-insensitive `http` scalar-wise, starting at `start`.
fn contains_http_scalar(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let word = bytes[i] as u32
            | (bytes[i + 1] as u32) << 8
            | (bytes[i + 2] as u32) << 16
            | (bytes[i + 3] as u32) << 24;
        if (word | 0x2020_2020) == HTTP_MASK_SCALAR {
            return true;
        }
        i += 1;
    }
    false
}

fn contains_www(bytes: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        simd::contains_www_impl(bytes)
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        simd::contains_www_impl(bytes)
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        contains_www_scalar(bytes)
    }
}

#[allow(dead_code)]
fn contains_www_scalar(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i] == b'w' && bytes[i + 1] == b'w' && bytes[i + 2] == b'w' {
            return true;
        }
        i += 1;
    }

    false
}

fn contains_xmpp(bytes: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        simd::contains_xmpp(bytes)
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        simd::contains_xmpp(bytes)
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        contains_pattern_scalar(bytes, b"xmpp")
    }
}

fn contains_mailto(bytes: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        simd::contains_mailto(bytes)
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        simd::contains_mailto(bytes)
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        contains_pattern_scalar(bytes, b"mailto")
    }
}

// WARNING: LLM-generated function
#[allow(dead_code)]
fn contains_pattern_scalar<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
    let mut i = 0;
    while i + N <= bytes.len() {
        if &bytes[i..i + N] == &pattern[..] {
            return true;
        }
        i += 1;
    }
    false
}

const INLINE_STARTER_BYTES_LUT: [u8; 16] = {
    let starters = b"\\`$*_~[]!<&\n\r";
    let mut lut = [0u8; 16];
    let mut i = 0;
    while i < starters.len() {
        let b = starters[i] as usize;
        lut[b & 0x0f] |= 1u8 << (b >> 4);
        i += 1;
    }
    lut
};

// Taken from pulldown-cmark
#[cfg(target_arch = "x86_64")]
mod simd {
    use super::INLINE_STARTER_BYTES_LUT;
    use super::next_starter_scalar;

    use core::arch::x86_64::*;

    const VECTOR_SIZE: usize = 16;
    const AVX2_SIZE: usize = 32;
    const AVX512_SIZE: usize = 64;

    // A mask for case-insensitive HTTP.
    const HTTP_MASK: u32 = u32::from_le_bytes([b'h' | 0x20, b't' | 0x20, b't' | 0x20, b'p' | 0x20]);

    /// Returns `true` if the next 16 bytes contain any potential inline construct delimiters.
    #[target_feature(enable = "ssse3")]
    fn contains_markdown_bytes_ssse3(bytes: &[u8], pos: usize) -> i32 {
        unsafe {
            let lut = _mm_loadu_si128(INLINE_STARTER_BYTES_LUT.as_ptr() as *const __m128i);
            let bitmask_lookup =
                _mm_setr_epi8(1, 2, 4, 8, 16, 32, 64, -128, -1, -1, -1, -1, -1, -1, -1, -1);
            let input = _mm_loadu_si128(bytes.as_ptr().add(pos) as *const __m128i);
            let bitset = _mm_shuffle_epi8(lut, input);
            let high = _mm_and_si128(_mm_srli_epi16(input, 4), _mm_set1_epi8(0x0f));
            let bitmask = _mm_shuffle_epi8(bitmask_lookup, high);
            let tmp = _mm_and_si128(bitset, bitmask);
            let result = _mm_cmpeq_epi8(tmp, bitmask);
            _mm_movemask_epi8(result)
        }
    }

    #[target_feature(enable = "avx2")]
    fn contains_markdown_bytes_avx2(bytes: &[u8], pos: usize) -> i32 {
        unsafe {
            let lut = _mm256_broadcastsi128_si256(_mm_loadu_si128(
                INLINE_STARTER_BYTES_LUT.as_ptr() as *const __m128i,
            ));
            let bitmask_lookup = _mm256_broadcastsi128_si256(_mm_setr_epi8(
                1, 2, 4, 8, 16, 32, 64, -128, -1, -1, -1, -1, -1, -1, -1, -1,
            ));
            let input = _mm256_loadu_si256(bytes.as_ptr().add(pos) as *const __m256i);
            let bitset = _mm256_shuffle_epi8(lut, input);
            let high = _mm256_and_si256(_mm256_srli_epi16(input, 4), _mm256_set1_epi8(0x0f));
            let bitmask = _mm256_shuffle_epi8(bitmask_lookup, high);
            let tmp = _mm256_and_si256(bitset, bitmask);
            let result = _mm256_cmpeq_epi8(tmp, bitmask);
            _mm256_movemask_epi8(result)
        }
    }

    #[target_feature(enable = "avx512bw")]
    fn contains_markdown_bytes_avx512(bytes: &[u8], pos: usize) -> u64 {
        unsafe {
            // `shuffle_epi8` works per 128-bit lane, so broadcast the lookup
            // tables to all four lanes.
            let lut = _mm512_broadcast_i32x4(_mm_loadu_si128(
                INLINE_STARTER_BYTES_LUT.as_ptr() as *const __m128i
            ));
            let bitmask_lookup = _mm512_broadcast_i32x4(_mm_setr_epi8(
                1, 2, 4, 8, 16, 32, 64, -128, -1, -1, -1, -1, -1, -1, -1, -1,
            ));
            let input = _mm512_loadu_si512(bytes.as_ptr().add(pos) as *const __m512i);
            let bitset = _mm512_shuffle_epi8(lut, input);
            let high = _mm512_and_si512(_mm512_srli_epi16(input, 4), _mm512_set1_epi8(0x0f));
            let bitmask = _mm512_shuffle_epi8(bitmask_lookup, high);
            let tmp = _mm512_and_si512(bitset, bitmask);
            _mm512_cmpeq_epi8_mask(tmp, bitmask)
        }
    }

    #[target_feature(enable = "ssse3")]
    fn next_starter_ssse3(bytes: &[u8], mut pos: usize) -> usize {
        let upperbound = bytes.len() - VECTOR_SIZE;
        while pos <= upperbound {
            let mask = contains_markdown_bytes_ssse3(bytes, pos);
            if mask != 0 {
                return pos + (mask as u32).trailing_zeros() as usize;
            }
            pos += VECTOR_SIZE;
        }
        next_starter_scalar(bytes, pos)
    }

    #[target_feature(enable = "avx2")]
    fn next_starter_avx2(bytes: &[u8], mut pos: usize) -> usize {
        let upperbound = bytes.len() - AVX2_SIZE;
        while pos <= upperbound {
            let mask = contains_markdown_bytes_avx2(bytes, pos);
            if mask != 0 {
                return pos + (mask as u32).trailing_zeros() as usize;
            }
            pos += AVX2_SIZE;
        }

        if bytes.len() - pos >= VECTOR_SIZE && is_x86_feature_detected!("ssse3") {
            next_starter_ssse3(bytes, pos)
        } else {
            next_starter_scalar(bytes, pos)
        }
    }

    #[target_feature(enable = "avx512bw")]
    fn next_starter_avx512(bytes: &[u8], mut pos: usize) -> usize {
        let upperbound = bytes.len() - AVX512_SIZE;
        while pos <= upperbound {
            let mask = contains_markdown_bytes_avx512(bytes, pos);
            if mask != 0 {
                return pos + mask.trailing_zeros() as usize;
            }
            pos += AVX512_SIZE;
        }

        // AVX-512BW hardware always has AVX2 and SSSE3, so no target feature checks needed
        if bytes.len() - pos >= AVX2_SIZE {
            next_starter_avx2(bytes, pos)
        } else if bytes.len() - pos >= VECTOR_SIZE {
            next_starter_ssse3(bytes, pos)
        } else {
            next_starter_scalar(bytes, pos)
        }
    }

    // CREDIT: Idea taken from pulldown-cmark
    pub fn next_starter_impl(bytes: &[u8], pos: usize) -> usize {
        let len = bytes.len();
        if is_x86_feature_detected!("avx512bw") && len - pos >= AVX512_SIZE {
            unsafe { next_starter_avx512(bytes, pos) }
        } else if is_x86_feature_detected!("avx2") && len - pos >= AVX2_SIZE {
            unsafe { next_starter_avx2(bytes, pos) }
        } else if is_x86_feature_detected!("ssse3") && len - pos >= VECTOR_SIZE {
            unsafe { next_starter_ssse3(bytes, pos) }
        } else {
            next_starter_scalar(bytes, pos)
        }
    }

    /// Returns `true` if the full bytes contains `http` (case-insensitive)
    #[target_feature(enable = "ssse3")]
    pub fn contains_http_ssse3(bytes: &[u8]) -> bool {
        unsafe {
            let is_uppercase = _mm_set1_epi8(0x20);
            let h = _mm_set1_epi8((b'h' | 0x20) as i8);
            let t = _mm_set1_epi8((b't' | 0x20) as i8);
            let p = _mm_set1_epi8((b'p' | 0x20) as i8);

            // A sliding window NFA for `h t t p`
            let mut i = 0usize;
            if bytes.len() >= 16 {
                let mut cur = _mm_loadu_si128(bytes.as_ptr() as *const __m128i);
                while i + 32 <= bytes.len() {
                    let next = _mm_loadu_si128(bytes.as_ptr().add(i + 16) as *const __m128i);
                    let c1 = _mm_alignr_epi8(next, cur, 1);
                    let c2 = _mm_alignr_epi8(next, cur, 2);
                    let c3 = _mm_alignr_epi8(next, cur, 3);
                    let m = _mm_and_si128(
                        _mm_and_si128(
                            _mm_cmpeq_epi8(_mm_or_si128(cur, is_uppercase), h),
                            _mm_cmpeq_epi8(_mm_or_si128(c1, is_uppercase), t),
                        ),
                        _mm_and_si128(
                            _mm_cmpeq_epi8(_mm_or_si128(c2, is_uppercase), t),
                            _mm_cmpeq_epi8(_mm_or_si128(c3, is_uppercase), p),
                        ),
                    );
                    if _mm_movemask_epi8(m) != 0 {
                        return true;
                    }
                    cur = next;
                    i += 16;
                }
            }

            // Scalar check remainder.
            while i + 4 <= bytes.len() {
                let word = bytes[i] as u32
                    | (bytes[i + 1] as u32) << 8
                    | (bytes[i + 2] as u32) << 16
                    | (bytes[i + 3] as u32) << 24;
                if (word | 0x2020_2020) == HTTP_MASK {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    /// Returns `true` if the full bytes contains `http` (case-insensitive)
    #[target_feature(enable = "avx2")]
    pub fn contains_http_avx2(bytes: &[u8]) -> bool {
        unsafe {
            let is_uppercase = _mm256_set1_epi8(0x20);
            let h = _mm256_set1_epi8((b'h' | 0x20) as i8);
            let t = _mm256_set1_epi8((b't' | 0x20) as i8);
            let p = _mm256_set1_epi8((b'p' | 0x20) as i8);

            // A sliding window NFA for `h t t p`
            let mut i = 0usize;
            if bytes.len() >= AVX2_SIZE {
                let mut cur = _mm256_loadu_si256(bytes.as_ptr() as *const __m256i);
                while i + 2 * AVX2_SIZE <= bytes.len() {
                    let next =
                        _mm256_loadu_si256(bytes.as_ptr().add(i + AVX2_SIZE) as *const __m256i);
                    // Stitches the high lane of `cur` to the low lane of `next` so that
                    // `alignr` slides bytes across the 128-bit lane boundary.
                    let shifted = _mm256_permute2x128_si256(cur, next, 0x21);
                    let c1 = _mm256_alignr_epi8(shifted, cur, 1);
                    let c2 = _mm256_alignr_epi8(shifted, cur, 2);
                    let c3 = _mm256_alignr_epi8(shifted, cur, 3);
                    let m = _mm256_and_si256(
                        _mm256_and_si256(
                            _mm256_cmpeq_epi8(_mm256_or_si256(cur, is_uppercase), h),
                            _mm256_cmpeq_epi8(_mm256_or_si256(c1, is_uppercase), t),
                        ),
                        _mm256_and_si256(
                            _mm256_cmpeq_epi8(_mm256_or_si256(c2, is_uppercase), t),
                            _mm256_cmpeq_epi8(_mm256_or_si256(c3, is_uppercase), p),
                        ),
                    );
                    if _mm256_movemask_epi8(m) != 0 {
                        return true;
                    }
                    cur = next;
                    i += AVX2_SIZE;
                }
            }

            // Scalar check remainder.
            while i + 4 <= bytes.len() {
                let word = bytes[i] as u32
                    | (bytes[i + 1] as u32) << 8
                    | (bytes[i + 2] as u32) << 16
                    | (bytes[i + 3] as u32) << 24;
                if (word | 0x2020_2020) == HTTP_MASK {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    /// Returns `true` if the full bytes contains `http` (case-insensitive)
    #[target_feature(enable = "avx512bw")]
    pub fn contains_http_avx512(bytes: &[u8]) -> bool {
        unsafe {
            let is_uppercase = _mm512_set1_epi8(0x20);
            let h = _mm512_set1_epi8((b'h' | 0x20) as i8);
            let t = _mm512_set1_epi8((b't' | 0x20) as i8);
            let p = _mm512_set1_epi8((b'p' | 0x20) as i8);

            // A sliding window NFA for `h t t p`
            let mut carry_h = 0u64;
            let mut carry_ht = 0u64;
            let mut carry_htt = 0u64;
            let mut i = 0usize;
            while i + AVX512_SIZE <= bytes.len() {
                let chunk = _mm512_loadu_si512(bytes.as_ptr().add(i) as *const __m512i);
                let lc = _mm512_or_si512(chunk, is_uppercase);
                let m_h = _mm512_cmpeq_epi8_mask(lc, h);
                let m_t = _mm512_cmpeq_epi8_mask(lc, t);
                let m_p = _mm512_cmpeq_epi8_mask(lc, p);

                let ht = ((m_h << 1) | carry_h) & m_t;
                let htt = ((ht << 1) | carry_ht) & m_t;
                if (((htt << 1) | carry_htt) & m_p) != 0 {
                    return true;
                }

                carry_h = m_h >> 63;
                carry_ht = ht >> 63;
                carry_htt = htt >> 63;
                i += AVX512_SIZE;
            }

            // Backtrack 3 bytes to not erroneously lose those bytes, then check remainder
            let mut i = i.saturating_sub(3);
            while i + 4 <= bytes.len() {
                let word = bytes[i] as u32
                    | (bytes[i + 1] as u32) << 8
                    | (bytes[i + 2] as u32) << 16
                    | (bytes[i + 3] as u32) << 24;
                if (word | 0x2020_2020) == HTTP_MASK {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    pub fn contains_http_impl(bytes: &[u8]) -> bool {
        let len = bytes.len();
        if len >= AVX512_SIZE && is_x86_feature_detected!("avx512bw") {
            unsafe { contains_http_avx512(bytes) }
        } else if len >= AVX2_SIZE && is_x86_feature_detected!("avx2") {
            unsafe { contains_http_avx2(bytes) }
        } else if is_x86_feature_detected!("ssse3") {
            unsafe { contains_http_ssse3(bytes) }
        } else {
            super::contains_http_scalar(bytes)
        }
    }

    // WARNING: These functions below are LLM-generated
    // TODO: check for www. not just www
    /// Returns `true` if the full bytes contains `www` (case-sensitive)
    #[target_feature(enable = "sse2")]
    pub fn contains_www_sse2(bytes: &[u8]) -> bool {
        unsafe {
            let w = _mm_set1_epi8(b'w' as i8);
            let mut i = 0usize;
            let mut carry = 0u32; // trailing 'w's of previous chunk, capped at 2

            while i + 16 <= bytes.len() {
                let chunk = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
                let m = _mm_movemask_epi8(_mm_cmpeq_epi8(chunk, w)) as u32 & 0xFFFF;

                // Three consecutive 'w's fully inside this chunk...
                if (m & (m >> 1) & (m >> 2)) != 0 {
                    return true;
                }
                // ...or spabytes.len(ing prev chunk's tail + this chunk's head.
                if carry + m.trailing_ones() >= 3 {
                    return true;
                }
                // Record this chunk's trailing run (bit 15 == lane 15 == last byte).
                carry = (m << 16).leading_ones().min(2);
                i += 16;
            }

            // Rewind over the last chunk's trailing run so matches spilling into the
            // tail are not missed, then scan the rest scalar-wise.
            i -= carry as usize;
            while i + 3 <= bytes.len() {
                if bytes[i] == b'w' && bytes[i + 1] == b'w' && bytes[i + 2] == b'w' {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    /// Returns `true` if the full bytes contains `www` (case-sensitive)
    #[target_feature(enable = "avx2")]
    pub fn contains_www_avx2(bytes: &[u8]) -> bool {
        unsafe {
            let w = _mm256_set1_epi8(b'w' as i8);
            let mut i = 0usize;
            let mut carry = 0u32; // trailing 'w's of previous chunk, capped at 2

            while i + AVX2_SIZE <= bytes.len() {
                let chunk = _mm256_loadu_si256(bytes.as_ptr().add(i) as *const __m256i);
                let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(chunk, w)) as u32;

                // Three consecutive 'w's fully inside this chunk...
                if (m & (m >> 1) & (m >> 2)) != 0 {
                    return true;
                }
                // ...or spanning prev chunk's tail + this chunk's head.
                if carry + m.trailing_ones() >= 3 {
                    return true;
                }
                // Record this chunk's trailing run (bit 31 == lane 31 == last byte).
                carry = m.leading_ones().min(2);
                i += AVX2_SIZE;
            }

            // Rewind over the last chunk's trailing run so matches spilling into the
            // tail are not missed, then scan the rest scalar-wise.
            i -= carry as usize;
            while i + 3 <= bytes.len() {
                if bytes[i] == b'w' && bytes[i + 1] == b'w' && bytes[i + 2] == b'w' {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    /// Returns `true` if the full bytes contains `www` (case-sensitive)
    #[target_feature(enable = "avx512bw")]
    pub fn contains_www_avx512(bytes: &[u8]) -> bool {
        unsafe {
            let w = _mm512_set1_epi8(b'w' as i8);
            let mut i = 0usize;
            let mut carry = 0u64; // trailing 'w's of previous chunk, capped at 2

            while i + AVX512_SIZE <= bytes.len() {
                let chunk = _mm512_loadu_si512(bytes.as_ptr().add(i) as *const __m512i);
                let m = _mm512_cmpeq_epi8_mask(chunk, w);

                // Three consecutive 'w's fully inside this chunk...
                if (m & (m >> 1) & (m >> 2)) != 0 {
                    return true;
                }
                // ...or spanning prev chunk's tail + this chunk's head.
                if carry + m.trailing_ones() as u64 >= 3 {
                    return true;
                }
                // Record this chunk's trailing run (bit 63 == lane 63 == last byte).
                carry = m.leading_ones().min(2) as u64;
                i += AVX512_SIZE;
            }

            // Rewind over the last chunk's trailing run so matches spilling into the
            // tail are not missed, then scan the rest scalar-wise.
            i -= carry as usize;
            while i + 3 <= bytes.len() {
                if bytes[i] == b'w' && bytes[i + 1] == b'w' && bytes[i + 2] == b'w' {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    pub fn contains_www_impl(bytes: &[u8]) -> bool {
        let len = bytes.len();
        if len >= AVX512_SIZE && is_x86_feature_detected!("avx512bw") {
            unsafe { contains_www_avx512(bytes) }
        } else if len >= AVX2_SIZE && is_x86_feature_detected!("avx2") {
            unsafe { contains_www_avx2(bytes) }
        } else if is_x86_feature_detected!("ssse3") {
            unsafe { contains_www_sse2(bytes) }
        } else {
            super::contains_www_scalar(bytes)
        }
    }

    /// Returns `true` if the full bytes contains `xmpp` (case-sensitive).
    pub fn contains_xmpp(bytes: &[u8]) -> bool {
        contains_pattern(bytes, b"xmpp")
    }

    /// Returns `true` if the full bytes contains `mailto` (case-sensitive).
    pub fn contains_mailto(bytes: &[u8]) -> bool {
        contains_pattern(bytes, b"mailto")
    }

    /// Sliding-window NFA over a fixed pattern, dispatching to the widest
    /// available vector width.
    fn contains_pattern<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
        let len = bytes.len();
        if len >= AVX512_SIZE && is_x86_feature_detected!("avx512bw") {
            unsafe { contains_pattern_avx512(bytes, pattern) }
        } else if len >= AVX2_SIZE && is_x86_feature_detected!("avx2") {
            unsafe { contains_pattern_avx2(bytes, pattern) }
        } else {
            unsafe { contains_pattern_sse2(bytes, pattern) }
        }
    }

    /// Sliding-window NFA over a fixed pattern, 128-bit lanes.
    #[target_feature(enable = "sse2")]
    fn contains_pattern_sse2<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
        let mut carries = [0u32; N];
        let mut i = 0usize;
        unsafe {
            while i + VECTOR_SIZE <= bytes.len() {
                let chunk = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
                let mut prev =
                    _mm_movemask_epi8(_mm_cmpeq_epi8(chunk, _mm_set1_epi8(pattern[0] as i8)))
                        as u32;
                for k in 1..N {
                    let m =
                        _mm_movemask_epi8(_mm_cmpeq_epi8(chunk, _mm_set1_epi8(pattern[k] as i8)))
                            as u32;
                    let cur = ((prev << 1) | carries[k - 1]) & m;
                    carries[k - 1] = prev >> 15;
                    prev = cur;
                }
                if prev != 0 {
                    return true;
                }
                i += VECTOR_SIZE;
            }
            let mut i = i.saturating_sub(N - 1);
            while i + N <= bytes.len() {
                if &bytes[i..i + N] == &pattern[..] {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    /// Sliding-window NFA over a fixed pattern, 256-bit lanes.
    #[target_feature(enable = "avx2")]
    fn contains_pattern_avx2<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
        let mut carries = [0u32; N];
        let mut i = 0usize;
        unsafe {
            while i + AVX2_SIZE <= bytes.len() {
                let chunk = _mm256_loadu_si256(bytes.as_ptr().add(i) as *const __m256i);
                let mut prev = _mm256_movemask_epi8(_mm256_cmpeq_epi8(
                    chunk,
                    _mm256_set1_epi8(pattern[0] as i8),
                )) as u32;
                for k in 1..N {
                    let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(
                        chunk,
                        _mm256_set1_epi8(pattern[k] as i8),
                    )) as u32;
                    let cur = ((prev << 1) | carries[k - 1]) & m;
                    carries[k - 1] = prev >> 31;
                    prev = cur;
                }
                if prev != 0 {
                    return true;
                }
                i += AVX2_SIZE;
            }
            let mut i = i.saturating_sub(N - 1);
            while i + N <= bytes.len() {
                if &bytes[i..i + N] == &pattern[..] {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    /// Sliding-window NFA over a fixed pattern, 512-bit lanes.
    #[target_feature(enable = "avx512bw")]
    fn contains_pattern_avx512<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
        let mut carries = [0u64; N];
        let mut i = 0usize;
        unsafe {
            while i + AVX512_SIZE <= bytes.len() {
                let chunk = _mm512_loadu_si512(bytes.as_ptr().add(i) as *const __m512i);
                let mut prev = _mm512_cmpeq_epi8_mask(chunk, _mm512_set1_epi8(pattern[0] as i8));
                for k in 1..N {
                    let m = _mm512_cmpeq_epi8_mask(chunk, _mm512_set1_epi8(pattern[k] as i8));
                    let cur = ((prev << 1) | carries[k - 1]) & m;
                    carries[k - 1] = prev >> 63;
                    prev = cur;
                }
                if prev != 0 {
                    return true;
                }
                i += AVX512_SIZE;
            }
            let mut i = i.saturating_sub(N - 1);
            while i + N <= bytes.len() {
                if &bytes[i..i + N] == &pattern[..] {
                    return true;
                }
                i += 1;
            }
        }
        false
    }
}

// WARNING: Entire module here is LLM-generated
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod simd {
    use super::INLINE_STARTER_BYTES_LUT;
    use super::next_starter_scalar;

    use core::arch::wasm32::*;

    const VECTOR_SIZE: usize = 16;

    const BITMASK_LOOKUP: [u8; 16] = [
        1, 2, 4, 8, 16, 32, 64, 128, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    #[target_feature(enable = "simd128")]
    unsafe fn compute_mask(bytes: &[u8], pos: usize) -> u16 {
        unsafe {
            let lut = v128_load(INLINE_STARTER_BYTES_LUT.as_ptr() as *const v128);
            let bitmask_lookup = v128_load(BITMASK_LOOKUP.as_ptr() as *const v128);
            let input = v128_load(bytes.as_ptr().add(pos) as *const v128);
            let lo = v128_and(input, i8x16_splat(0x0f));
            let bitset = i8x16_swizzle(lut, lo);
            let hi = v128_and(u8x16_shr(input, 4), i8x16_splat(0x0f));
            let bitmask = i8x16_swizzle(bitmask_lookup, hi);
            let tmp = v128_and(bitset, bitmask);
            let result = i8x16_eq(tmp, bitmask);
            i8x16_bitmask(result)
        }
    }

    #[target_feature(enable = "simd128")]
    unsafe fn next_starter_simd128(bytes: &[u8], mut pos: usize) -> usize {
        unsafe {
            let upperbound = bytes.len() - VECTOR_SIZE;
            while pos <= upperbound {
                let mask = compute_mask(bytes, pos);
                if mask != 0 {
                    return pos + mask.trailing_zeros() as usize;
                }
                pos += VECTOR_SIZE;
            }
        }
        next_starter_scalar(bytes, pos)
    }

    pub fn next_starter_impl(bytes: &[u8], pos: usize) -> usize {
        let len = bytes.len();
        if len - pos >= VECTOR_SIZE {
            unsafe { next_starter_simd128(bytes, pos) }
        } else {
            next_starter_scalar(bytes, pos)
        }
    }

    /// Returns `true` if the full bytes contains `http` (case-insensitive)
    #[target_feature(enable = "simd128")]
    unsafe fn contains_http_simd128(bytes: &[u8]) -> bool {
        unsafe {
            let is_uppercase = i8x16_splat(0x20);
            let h = i8x16_splat((b'h' | 0x20) as i8);
            let t = i8x16_splat((b't' | 0x20) as i8);
            let p = i8x16_splat((b'p' | 0x20) as i8);

            // A sliding window NFA for `h t t p`
            let mut carry_h = 0u16;
            let mut carry_ht = 0u16;
            let mut carry_htt = 0u16;
            let mut i = 0usize;
            while i + VECTOR_SIZE <= bytes.len() {
                let chunk = v128_load(bytes.as_ptr().add(i) as *const v128);
                let lc = v128_or(chunk, is_uppercase);
                let m_h = i8x16_bitmask(i8x16_eq(lc, h));
                let m_t = i8x16_bitmask(i8x16_eq(lc, t));
                let m_p = i8x16_bitmask(i8x16_eq(lc, p));

                let ht = ((m_h << 1) | carry_h) & m_t;
                let htt = ((ht << 1) | carry_ht) & m_t;
                if (((htt << 1) | carry_htt) & m_p) != 0 {
                    return true;
                }

                carry_h = m_h >> 15;
                carry_ht = ht >> 15;
                carry_htt = htt >> 15;
                i += VECTOR_SIZE;
            }

            // Backtrack 3 bytes to not erroneously lose those bytes, then check remainder
            let mut i = i.saturating_sub(3);
            while i + 4 <= bytes.len() {
                let word = bytes[i] as u32
                    | (bytes[i + 1] as u32) << 8
                    | (bytes[i + 2] as u32) << 16
                    | (bytes[i + 3] as u32) << 24;
                if (word | 0x2020_2020) == super::HTTP_MASK_SCALAR {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    pub fn contains_http_impl(bytes: &[u8]) -> bool {
        if bytes.len() >= VECTOR_SIZE {
            unsafe { contains_http_simd128(bytes) }
        } else {
            super::contains_http_scalar(bytes)
        }
    }

    /// Sliding-window NFA over a fixed pattern, 128-bit lanes.
    #[target_feature(enable = "simd128")]
    unsafe fn contains_pattern_simd128<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
        let mut carries = [0u16; N];
        let mut i = 0usize;
        unsafe {
            while i + VECTOR_SIZE <= bytes.len() {
                let chunk = v128_load(bytes.as_ptr().add(i) as *const v128);
                let mut prev = i8x16_bitmask(i8x16_eq(chunk, i8x16_splat(pattern[0] as i8)));
                for k in 1..N {
                    let m = i8x16_bitmask(i8x16_eq(chunk, i8x16_splat(pattern[k] as i8)));
                    let cur = ((prev << 1) | carries[k - 1]) & m;
                    carries[k - 1] = prev >> 15;
                    prev = cur;
                }
                if prev != 0 {
                    return true;
                }
                i += VECTOR_SIZE;
            }
            let mut i = i.saturating_sub(N - 1);
            while i + N <= bytes.len() {
                if &bytes[i..i + N] == &pattern[..] {
                    return true;
                }
                i += 1;
            }
        }
        false
    }

    fn contains_pattern<const N: usize>(bytes: &[u8], pattern: &[u8; N]) -> bool {
        if bytes.len() >= VECTOR_SIZE {
            unsafe { contains_pattern_simd128(bytes, pattern) }
        } else {
            super::contains_pattern_scalar(bytes, pattern)
        }
    }

    /// Returns `true` if the full bytes contains `www` (case-sensitive)
    pub fn contains_www_impl(bytes: &[u8]) -> bool {
        contains_pattern(bytes, b"www")
    }

    /// Returns `true` if the full bytes contains `xmpp` (case-sensitive).
    pub fn contains_xmpp(bytes: &[u8]) -> bool {
        contains_pattern(bytes, b"xmpp")
    }

    /// Returns `true` if the full bytes contains `mailto` (case-sensitive).
    pub fn contains_mailto(bytes: &[u8]) -> bool {
        contains_pattern(bytes, b"mailto")
    }
}
