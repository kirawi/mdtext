use std::{
    borrow::Cow,
    ops::{Bound, Range, RangeBounds},
};

use crate::block::Span;

/// A physical byte position paired with the content span that owns it.
/// A useful optimization to bookmark and avoid traversal cost.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Location {
    pub pos: usize,
    pub span_idx: usize,
}

// NOTE: the `Ld` name has no meaning. it was just easy to type

/// A cursor over the input spans and underlying buffer for parsing.
pub struct Ld<'a, 's> {
    bytes: &'a [u8],
    spans: &'s [Span],
    /// Whether all spans are contiguous (allows some optimizations)
    contiguous: bool,
    /// The current cursor position into `bytes`.
    pub pos: usize,
    /// Current span index into `spans`.
    pub span_idx: usize,
    /// End of last span (one past the final content byte).
    pub len: usize,
}

impl<'a, 's> Ld<'a, 's> {
    pub fn new(bytes: &'a [u8], spans: &'s [Span]) -> Self {
        let len = spans.last().map(|s| s.end).unwrap_or(0);
        let pos = spans.first().map(|s| s.start).unwrap_or(0);
        let contiguous = spans.windows(2).all(|w| w[0].end == w[1].start);
        Self {
            bytes,
            spans,
            contiguous,
            pos,
            span_idx: 0,
            len,
        }
    }

    /// The underlying document buffer.
    #[inline]
    pub fn buf(&self) -> &'a [u8] {
        self.bytes
    }

    #[inline]
    pub fn text(&self) -> &'a str {
        // SAFETY: `self.bytes` is derived from valid ASCII byte offsets on an already valid UTF-8 string, and
        // thus will not be invalid UTF-8 here.
        unsafe { std::str::from_utf8_unchecked(self.bytes) }
    }

    #[inline]
    pub fn text_here(&self) -> &'a str {
        // SAFETY: `self.bytes` is derived from valid ASCII byte offsets on an already valid UTF-8 string, and
        // thus will not be invalid UTF-8 here.
        unsafe { std::str::from_utf8_unchecked(&self.bytes[self.pos..]) }
    }

    /// The byte at the cursor, or `None` once the end has been reached.
    pub fn current(&self) -> Option<u8> {
        if self.pos == self.len {
            return None;
        }
        Some(self.buf()[self.pos])
    }

    /// The UTF-8 char at the cursor, or `None` once the end has been reached.
    pub fn current_char(&self) -> Option<char> {
        if self.pos >= self.len {
            return None;
        }
        let b = self.buf()[self.pos];
        if b.is_ascii() {
            Some(b as char)
        } else {
            // SAFETY: doc content is valid UTF-8.
            unsafe { std::str::from_utf8_unchecked(&self.bytes[self.pos..self.len]) }
                .chars()
                .next()
        }
    }

    #[inline]
    pub fn current_unchecked(&self) -> u8 {
        self.get_unchecked(self.pos)
    }

    /// Returns the byte after the cursor without advancing.
    pub fn peek_next(&self) -> Option<u8> {
        self.buf().get(self.pos + 1).copied()
    }

    /// Moves the cursor to `pos`.
    #[inline]
    pub fn seek(&mut self, pos: usize) {
        debug_assert!(pos <= self.len);
        self.pos = pos;
    }

    /// Advances the cursor by `n` bytes.
    #[inline]
    pub fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    /// Start of the first span (begin of content).
    pub fn content_start(&self) -> usize {
        self.spans.first().map(|s| s.start).unwrap_or(0)
    }

    /// The current cursor position and owning content span.
    #[inline]
    pub fn location(&self) -> Location {
        Location {
            pos: self.pos,
            span_idx: self.span_idx,
        }
    }

    /// Moves the cursor to `location`.
    #[inline]
    pub fn seek_location(&mut self, location: Location) {
        self.pos = location.pos;
        self.span_idx = location.span_idx;
    }

    /// Returns the byte at the physical position `idx`.
    pub fn get_unchecked(&self, idx: usize) -> u8 {
        // SAFETY: only called after bounds checking (LLVM might already have ellided bounds check on call
        // anyway, so this might be unnecessary)
        // TODO: decompile and check! no guesswork!
        debug_assert!(idx < self.buf().len());
        *unsafe { self.buf().get_unchecked(idx) }
    }

    pub fn get(&self, idx: usize) -> Option<u8> {
        self.buf().get(idx).copied()
    }

    /// Whether paragraph content occupies one physically contiguous range.
    #[inline]
    pub fn is_contiguous(&self) -> bool {
        self.contiguous
    }

    /// Content spans at and after `index`.
    #[inline]
    pub fn spans_from(&self, index: usize) -> &[Span] {
        &self.spans[index..]
    }

    /// Construct a location for a position known to be at or after the current cursor span.
    #[inline]
    pub fn location_at(&self, pos: usize) -> Location {
        self.location_at_from(pos, self.span_idx)
    }

    /// Construct a location for a position known to be at or after `from_span_idx`.
    #[inline]
    pub fn location_at_from(&self, pos: usize, from_span_idx: usize) -> Location {
        let suffix = &self.spans[from_span_idx..];
        let offset = suffix.partition_point(|span| span.end <= pos);
        let index = from_span_idx + offset;
        debug_assert!(
            self.spans
                .get(index)
                .is_some_and(|span| span.contains(&pos)),
            "location is invalid!"
        );

        Location {
            pos,
            span_idx: index,
        }
    }

    /// Byte at a valid content location, or `None` at the terminal end.
    #[inline]
    pub fn byte_at_location(&self, location: Location) -> Option<u8> {
        let span = self.spans.get(location.span_idx)?;

        // TODO: invariant might prevent this from ever happening anyway, so no need Option?
        if location.pos < span.end {
            debug_assert!(location.pos >= span.start);
            Some(self.bytes[location.pos])
        } else {
            None
        }
    }

    /// Returns the unicode char at the pointed location
    pub fn char_at(&self, location: Location) -> Option<char> {
        let span = self.spans.get(location.span_idx)?;
        if location.pos >= span.end {
            return None;
        }

        let bytes = &self.bytes[location.pos..span.end];
        if bytes[0].is_ascii() {
            Some(bytes[0] as char)
        } else {
            unsafe { std::str::from_utf8_unchecked(bytes) }
                .chars()
                .next()
        }
    }

    /// Returns the unicode char prior to the pointed location
    pub fn char_before_location(&self, location: Location) -> Option<char> {
        let span = self.spans.get(location.span_idx)?;
        let end = if location.pos > span.start {
            location.pos
        } else {
            self.spans.get(location.span_idx.checked_sub(1)?)?.end
        };

        if end == 0 || end > self.len {
            return None;
        }
        let buf = self.buf();
        // ASCII fast path — no UTF-8 validation needed.
        if buf[end - 1] < 0x80 {
            return Some(buf[end - 1] as char);
        }
        let mut start = end - 1;
        while start > 0 && end - start < 4 && buf[start] & 0xC0 == 0x80 {
            start -= 1;
        }
        unsafe { std::str::from_utf8_unchecked(&buf[start..end]) }
            .chars()
            .next()
    }

    /// Move to the preceding byte of logical content. At a span boundary this
    /// jumps to the prior span instead of entering a stripped prefix gap.
    #[inline]
    pub fn prev_by_location(&self, location: &mut Location) -> bool {
        let span = &self.spans[location.span_idx];
        debug_assert!(location.pos >= span.start && location.pos <= span.end);
        if location.pos > span.start {
            location.pos -= 1;
            return true;
        }
        if let Some(previous_idx) = location.span_idx.checked_sub(1) {
            location.span_idx = previous_idx;
            location.pos = self.spans[previous_idx].end - 1;
            true
        } else {
            false
        }
    }

    /// Advance by one byte of logical paragraph content, jumping over any
    /// stripped prefix gap. Returns false after moving to the terminal end.
    #[inline]
    pub fn advance_location(&self, location: &mut Location) -> bool {
        // SAFETY: Only valid Locations are EVER constructed via the methods here`
        let span = unsafe { self.spans.get_unchecked(location.span_idx) };
        debug_assert!(location.pos >= span.start && location.pos < span.end);

        // Bounds check
        if location.pos + 1 < span.end {
            location.pos += 1;
            return true;
        }

        if let Some(next) = self.spans.get(location.span_idx + 1) {
            location.span_idx += 1;
            location.pos = next.start;
            true
        } else {
            location.pos = span.end;
            false
        }
    }

    /// Advance by `count` logical content bytes.
    #[inline]
    pub fn advance_location_by(&self, location: &mut Location, count: usize) -> bool {
        for _ in 0..count {
            if !self.advance_location(location) {
                return false;
            }
        }
        true
    }

    /// Find a byte without inspecting stripped prefix gaps.
    pub fn find_byte_from_location(&self, byte: u8, location: Location) -> Option<Location> {
        for (offset, span) in self.spans[location.span_idx..].iter().enumerate() {
            let span_idx = location.span_idx + offset;
            let start = if offset == 0 {
                location.pos
            } else {
                span.start
            };
            if start >= span.end {
                continue;
            }
            if let Some(relative) = memchr::memchr(byte, &self.bytes[start..span.end]) {
                return Some(Location {
                    pos: start + relative,
                    span_idx,
                });
            }
        }
        None
    }

    /// Test a byte string against logical content at `location`.
    pub fn starts_with_at_location(&self, mut location: Location, pattern: &[u8]) -> bool {
        for &expected in pattern {
            if self.byte_at_location(location) != Some(expected) {
                return false;
            }
            self.advance_location(&mut location);
        }
        true
    }

    /// Find a byte string without inspecting stripped prefix gaps.
    pub fn find_subslice_from_location(
        &self,
        pattern: &[u8],
        mut location: Location,
    ) -> Option<Location> {
        debug_assert!(!pattern.is_empty());
        while let Some(candidate) = self.find_byte_from_location(pattern[0], location) {
            if self.starts_with_at_location(candidate, pattern) {
                return Some(candidate);
            }
            location = candidate;
            if !self.advance_location(&mut location) {
                return None;
            }
        }
        None
    }

    /// Returns a string within the slice.
    pub fn slice<R: RangeBounds<usize>>(&self, range: R) -> Cow<'a, str> {
        let range = resolve_range(range, self.len);
        if range.start >= range.end {
            return Cow::Borrowed("");
        }
        if self.contiguous {
            // SAFETY: callers only ever use ranges over valid UTF-8 doc content.
            return Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&self.bytes[range]) });
        }
        // Non-contiguous: check if range is within a single span inline.
        for span in self.spans {
            if range.start >= span.start && range.end <= span.end {
                // SAFETY: callers only ever use ranges over valid UTF-8 doc content.
                return Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&self.bytes[range]) });
            }
        }
        // Need to concatenate if it's multiline
        // TODO: should be eliminated once we ensure events are per-line (UNLESS contiguous in bytes)?
        self.slice_concat(range)
    }

    #[cold]
    fn slice_concat(&self, range: Range<usize>) -> Cow<'a, str> {
        let mut result = String::with_capacity(range.end - range.start);
        for span in self.spans {
            if range.end <= span.start || range.start >= span.end {
                continue;
            }
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);

            // SAFETY: valid UTF-8 doc content.
            result.push_str(unsafe { std::str::from_utf8_unchecked(&self.bytes[start..end]) });
        }
        Cow::Owned(result)
    }

    // Only on a single slice
    // TODO: replace usage with slice() prob
    #[inline]
    pub fn borrow<R: RangeBounds<usize>>(&self, range: R) -> Cow<'a, str> {
        let range = resolve_range(range, self.len);
        debug_assert!(range.start < range.end); // Should never be more than one past the end!

        // If check is permissive just in case (but should never happen!)
        if range.start >= range.end {
            return Cow::Borrowed("");
        }
        // SAFETY: callers only ever use ranges over valid UTF-8 doc content.
        Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&self.bytes[range]) })
    }

    #[inline]
    pub fn has_next_line(&self) -> bool {
        self.span_idx + 1 < self.spans.len()
    }

    /// Advance the cursor's span index to the next line/span, returning its start position if present.
    #[inline]
    pub fn advance_line(&mut self) -> bool {
        self.span_idx += 1;
        if let Some(p) = self.spans.get(self.span_idx).map(|s| s.start) {
            self.pos = p;
            true
        } else {
            // Jump to end of line (i.e. end of inline text to parse)!
            // This function is only ever called when text on the line cannot be read as any other
            // event anyway.
            self.pos = self.len;
            false
        }
    }
}

/// Resolves any range bounds into a regular range, clamped to `len`.
fn resolve_range<R: RangeBounds<usize>>(range: R, len: usize) -> Range<usize> {
    let start = match range.start_bound() {
        Bound::Included(&s) => s,
        Bound::Excluded(&s) => s + 1,
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&e) => e + 1,
        Bound::Excluded(&e) => e,
        Bound::Unbounded => len,
    }
    .min(len);
    start..end
}

#[inline(always)]
pub fn bytes_has_nul(bytes: &[u8]) -> bool {
    if bytes.len() >= 16 {
        memchr::memchr(b'\0', bytes).is_some()
    } else {
        bytes.contains(&b'\0')
    }
}
