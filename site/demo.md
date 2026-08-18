# Markdown, a piece at a time

This page is not reparsing a finished document after every keystroke. The text at left is arriving in small chunks. **mdtext** retains only what it still needs and releases events as blocks become complete.

> Watch the output pause while a paragraph is unfinished, then advance when the blank line arrives.

| Property | This demonstration |
| :--- | :--- |
| Input | Incremental UTF-8 chunks |
| Output | Incremental HTML deltas |
| Runtime | Rust compiled to WebAssembly |

- [x] CommonMark parsing
- [x] GitHub-flavored extensions
- [x] Streaming input and output

```rust
let mut parser = mdtext::Parser::new();
let (events, consumed) = parser.feed_chunk("# streamed\n");
```

Inline math remains semantic HTML: $x^2 + y^2$.
