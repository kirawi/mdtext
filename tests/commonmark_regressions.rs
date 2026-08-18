// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use mdtext::{Event, ListKind, Options, Parser, Tag};

fn parse(markdown: &str) -> Vec<Event<'_>> {
    Parser::parse_str(markdown, Options::empty())
}

fn first_code_span<'e, 'a>(events: &'e [Event<'a>]) -> Vec<&'e str> {
    let start = events
        .iter()
        .position(|event| *event == Event::Start(Tag::CodeSpan))
        .expect("expected a code span");
    events[start + 1..]
        .iter()
        .take_while(|event| **event != Event::End)
        .filter_map(|event| match event {
            Event::Code(content) => Some(content.as_ref()),
            _ => None,
        })
        .collect()
}

#[test]
fn interrupting_blocks_close_unmatched_block_quotes_first() {
    assert_eq!(
        parse("> a\n# b"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Text("a".into()),
            Event::End,
            Event::End,
            Event::Start(Tag::Heading(1)),
            Event::Text("b".into()),
            Event::End,
        ]
    );

    assert_eq!(
        parse("> foo\n---"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
            Event::ThematicBreak,
        ]
    );

    assert_eq!(
        parse("> foo\n~~~\nx\n~~~\n"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("x\n".into()),
            Event::End,
        ]
    );
}

#[test]
fn type_7_html_block_is_not_a_lazy_container_continuation() {
    assert_eq!(
        parse("6. Simplify\ncontinued\n</thinking>\n"),
        vec![
            Event::Start(Tag::List(ListKind::Ordered(6))),
            Event::Start(Tag::Item),
            Event::Start(Tag::Paragraph),
            Event::Text("Simplify".into()),
            Event::SoftBreak,
            Event::Text("continued".into()),
            Event::End,
            Event::End,
            Event::End,
            Event::Start(Tag::HtmlBlock),
            Event::Html("</thinking>\n".into()),
            Event::End,
        ]
    );

    // Type-7 HTML still cannot interrupt an ordinary paragraph (§4.6).
    assert_eq!(
        parse("foo\n</thinking>\n"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("foo".into()),
            Event::SoftBreak,
            Event::Html("</thinking>".into()),
            Event::End,
        ]
    );
}

#[test]
fn bullet_item_content_is_not_scanned_as_a_table_delimiter() {
    assert_eq!(
        parse("intro:\n- |M| = x\n- :value\n"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("intro:".into()),
            Event::End,
            Event::Start(Tag::List(ListKind::Unordered)),
            Event::Start(Tag::Item),
            Event::Start(Tag::Paragraph),
            Event::Text("|M| = x".into()),
            Event::End,
            Event::End,
            Event::Start(Tag::Item),
            Event::Start(Tag::Paragraph),
            Event::Text(":value".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn code_span_trimming_uses_logical_content_across_indent_gaps() {
    for line_ending in ["\n", "\r", "\r\n"] {
        let markdown = format!("` is concave up.{line_ending}    ` after `b`");
        assert_eq!(
            parse(&markdown),
            vec![
                Event::Start(Tag::Paragraph),
                Event::Start(Tag::CodeSpan),
                Event::Code("is concave up.".into()),
                Event::End,
                Event::Text(" after ".into()),
                Event::Start(Tag::CodeSpan),
                Event::Code("b".into()),
                Event::End,
                Event::End,
            ],
            "line ending {line_ending:?}"
        );
    }

    assert_eq!(
        parse("- ` x\n  ` after `b`"),
        vec![
            Event::Start(Tag::List(ListKind::Unordered)),
            Event::Start(Tag::Item),
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::CodeSpan),
            Event::Code("x".into()),
            Event::End,
            Event::Text(" after ".into()),
            Event::Start(Tag::CodeSpan),
            Event::Code("b".into()),
            Event::End,
            Event::End,
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("> ` x\n> y ` after `b`"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::CodeSpan),
            Event::Code("x".into()),
            Event::Code(" ".into()),
            Event::Code("y".into()),
            Event::End,
            Event::Text(" after ".into()),
            Event::Start(Tag::CodeSpan),
            Event::Code("b".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn unmarked_blank_line_separates_block_quotes() {
    assert_eq!(
        parse("> a\n\n> b"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Text("a".into()),
            Event::End,
            Event::End,
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Text("b".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn list_marker_delimiters_define_distinct_lists() {
    let bullets = parse("- a\n+ b\n* c");
    assert_eq!(
        bullets
            .iter()
            .filter(|event| **event == Event::Start(Tag::List(ListKind::Unordered)))
            .count(),
        3
    );

    let ordered = parse("1. a\n2) b");
    assert_eq!(
        ordered
            .iter()
            .filter(|event| matches!(event, Event::Start(Tag::List(ListKind::Ordered(_)))))
            .count(),
        2
    );
}

/// A less-indented marker of the same type is a sibling, not a new list.
///
/// The CommonMark spec (§5.3, example 312) shows that items at indents
/// 0,1,2,3,2,1,0 are all siblings in a single list.  The `M ≤ I` rule
/// (where M is the first item's marker indent) incorrectly rejects
/// markers whose indent is less than M.  These cases all start with an
/// indented first item so that a subsequent less-indented marker has
/// I < M, which triggers the false "Neither" classification.
#[test]
fn dedented_marker_is_sibling_not_new_list() {
    let cases: &[(&str, &str)] = &[
        ("  - foo\n- bar\n", "indent 2→0"),
        ("   - foo\n- bar\n", "indent 3→0"),
        ("  - foo\n - bar\n", "indent 2→1"),
        ("  - foo\n\n- bar\n", "indent 2→0 with blank line"),
    ];

    for (markdown, label) in cases {
        let events = parse(markdown);
        let list_count = events
            .iter()
            .filter(|event| **event == Event::Start(Tag::List(ListKind::Unordered)))
            .count();
        let item_count = events
            .iter()
            .filter(|event| **event == Event::Start(Tag::Item))
            .count();
        assert_eq!(
            list_count, 1,
            "{label:?}: expected 1 list, got {list_count} — events: {events:?}"
        );
        assert_eq!(
            item_count, 2,
            "{label:?}: expected 2 items, got {item_count} — events: {events:?}"
        );
    }
}

#[test]
fn empty_list_items_do_not_interrupt_paragraphs() {
    for markdown in ["foo\n*\n", "foo\n1.\n"] {
        let events = parse(markdown);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::List(_)))),
            "{markdown:?} unexpectedly opened a list: {events:?}"
        );
    }
}

#[test]
fn partial_item_indentation_remains_inline_content() {
    let events = parse("- `abc\nx\n `tail");
    assert_eq!(first_code_span(&events), ["abc", " ", "x", " ", " "]);

    let events = parse("- a `foo\nbar\n #`tail");
    assert_eq!(first_code_span(&events), ["foo", " ", "bar", " ", " #"]);
}

#[test]
fn paragraph_continuations_strip_all_leading_indentation() {
    assert_eq!(
        parse("Foo\n        bar"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("Foo".into()),
            Event::SoftBreak,
            Event::Text("bar".into()),
            Event::End,
        ]
    );

    let events = parse("Tokenizer` is used to:\n        * one\n    * `x`");
    assert_eq!(
        first_code_span(&events),
        ["is used to:", " ", "* one", " ", "*"]
    );
}

#[test]
fn wide_list_marker_padding_preserves_indented_code_surplus() {
    assert_eq!(
        parse("+         1\n"),
        vec![
            Event::Start(Tag::List(ListKind::Unordered)),
            Event::Start(Tag::Item),
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("    1\n".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn literal_blocks_preserve_source_line_endings() {
    assert_eq!(
        parse("<!--x-->\n"),
        vec![
            Event::Start(Tag::HtmlBlock),
            Event::Html("<!--x-->\n".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("<p\nx\n\n"),
        vec![
            Event::Start(Tag::HtmlBlock),
            Event::Html("<p\n".into()),
            Event::Html("x\n".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("<p\nx"),
        vec![
            Event::Start(Tag::HtmlBlock),
            Event::Html("<p\n".into()),
            Event::Html("x".into()),
            Event::End,
        ]
    );

    assert_eq!(
        parse("~~~\nx"),
        vec![
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("x".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("    x"),
        vec![
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("x".into()),
            Event::End,
        ]
    );
}

#[test]
fn all_commonmark_line_endings_are_recognized() {
    let expected = vec![
        Event::Start(Tag::Paragraph),
        Event::Text("a".into()),
        Event::SoftBreak,
        Event::Text("b".into()),
        Event::SoftBreak,
        Event::Text("c".into()),
        Event::End,
    ];
    assert_eq!(parse("a\rb\r\nc"), expected);
}

#[test]
fn nul_is_replaced_with_the_replacement_character() {
    assert_eq!(
        parse("a\0b"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("a\u{fffd}b".into()),
            Event::End,
        ]
    );
}

#[test]
fn inline_html_requires_a_nonempty_unquoted_attribute_value() {
    assert_eq!(
        parse("x <a href=>"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("x <a href=>".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("x <a href = value>"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("x ".into()),
            Event::Html("<a href = value>".into()),
            Event::End,
        ]
    );
}

#[test]
fn multiline_inline_constructs_skip_container_prefix_gaps_and_commit_their_span() {
    assert_eq!(
        parse("> <a\n> href=x>tail"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Html("<a\nhref=x>".into()),
            Event::Text("tail".into()),
            Event::End,
            Event::End,
        ]
    );

    // The link closer is on the second span. Text on that span and the next
    // line must be consumed exactly once after the speculative scan commits.
    assert_eq!(
        parse("[x](\n/url) tail\ny"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Link {
                url: "/url".into(),
                title: None,
            }),
            Event::Text("x".into()),
            Event::End,
            Event::Text(" tail".into()),
            Event::SoftBreak,
            Event::Text("y".into()),
            Event::End,
        ]
    );

    assert_eq!(
        parse("> [x](\n> /url) tail\n> y"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Link {
                url: "/url".into(),
                title: None,
            }),
            Event::Text("x".into()),
            Event::End,
            Event::Text(" tail".into()),
            Event::SoftBreak,
            Event::Text("y".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn invalid_link_destination_is_not_reinterpreted_as_a_title() {
    let source = "[x]((foo bar))";
    let events = parse(source);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Link { .. })))
    );
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Event::Text(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(text, source);
}

#[test]
fn delimiter_flanking_uses_logical_content_at_container_boundaries() {
    // The byte immediately before the second underscore in the source is a
    // stripped `>` prefix. Logically it is the preceding line ending, so this
    // must classify exactly like `_a\n_ b`.
    assert_eq!(
        parse("> _a\n>_ b"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::Paragraph),
            Event::Text("_".into()),
            Event::Text("a".into()),
            Event::SoftBreak,
            Event::Text("_ b".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn tabs_affect_block_structure_but_internal_tabs_remain_literal() {
    assert_eq!(
        parse(">\t\tfoo"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("  foo".into()),
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("-\t\tfoo"),
        vec![
            Event::Start(Tag::List(ListKind::Unordered)),
            Event::Start(Tag::Item),
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("  foo".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );

    // The block quote's `> ` prefix places its content at column 2. The first
    // tab advances to column 4 and the second to column 8, so stripping four
    // columns from this interior blank code line must leave two spaces.
    assert_eq!(
        parse(">     a\n> \t\t\n>     b\n"),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::CodeBlock(None)),
            Event::Code("a\n".into()),
            Event::Code("  \n".into()),
            Event::Code("b\n".into()),
            Event::End,
            Event::End,
        ]
    );

    assert!(parse("\tfoo\tbar").contains(&Event::Code("foo\tbar".into())));
}

#[test]
fn thematic_break_takes_precedence_over_a_list_marker_prefix() {
    assert_eq!(parse("- ---\n"), vec![Event::ThematicBreak]);
    assert_eq!(
        parse("> - ---\n"),
        vec![Event::Start(Tag::Quote), Event::ThematicBreak, Event::End,]
    );
}

#[test]
fn code_span_at_paragraph_start_has_no_empty_text() {
    // When a code span begins at the very start of inline content (or
    // immediately after another construct with no text in between),
    // `tick_start == pending_text_start`. The guard in `scan_code_span`
    // must skip the text-flush in that case — otherwise an empty
    // `Event::Text("")` is emitted before the code event.
    assert_eq!(
        parse("`a`"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::CodeSpan),
            Event::Code("a".into()),
            Event::End,
            Event::End,
        ]
    );

    // Code span immediately after an entity reference — `push_node` from
    // the entity leaves `pending_text_start` at the backtick position.
    assert_eq!(
        parse("&amp;`x`"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("&".into()),
            Event::Start(Tag::CodeSpan),
            Event::Code("x".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn fenced_code_info_strings_are_decoded() {
    assert_eq!(
        parse("```a\\|b&amp;c\nx\n```\n"),
        vec![
            Event::Start(Tag::CodeBlock(Some("a|b&c".into()))),
            Event::Code("x\n".into()),
            Event::End,
        ]
    );
}

/// A hard line break requires a following line *within* the same paragraph.
/// At the end of the final line of a paragraph the `\` (or trailing spaces)
/// must not produce a break event, and the `\` itself is literal text.
/// Covers the `advance_line` failure path: every early return there must keep
/// `pending_text_start` in sync or the final flush emits stray newline bytes.
#[test]
fn paragraph_final_line_ending_produces_no_break_event() {
    for prefix in ["", "- ", "> "] {
        // Backslash before the paragraph-final newline is a literal `\`.
        for ending in ["\n", "\r\n", "\r"] {
            let markdown = format!("{prefix}foo\\{ending}");
            let events = parse(&markdown);
            assert!(
                !events.contains(&Event::HardBreak) && !events.contains(&Event::SoftBreak),
                "{markdown:?}: unexpected break event: {events:?}"
            );
            let text: String = events
                .iter()
                .filter_map(|event| match event {
                    Event::Text(text) => Some(text.as_ref()),
                    _ => None,
                })
                .collect();
            assert_eq!(text, "foo\\", "{markdown:?}: {events:?}");
        }

        // Trailing-space hard break at the paragraph end: no break, no spaces.
        let markdown = format!("{prefix}foo  ");
        let events = parse(&markdown);
        assert!(
            !events.contains(&Event::HardBreak) && !events.contains(&Event::SoftBreak),
            "{markdown:?}: unexpected break event: {events:?}"
        );
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "foo", "{markdown:?}: {events:?}");
    }

    // A bare paragraph-final newline must not leak as a stray Text node.
    for markdown in ["foo\n", "foo\r\n", "foo\r"] {
        let events = parse(markdown);
        assert_eq!(
            events,
            vec![
                Event::Start(Tag::Paragraph),
                Event::Text("foo".into()),
                Event::End,
            ],
            "{markdown:?}"
        );
    }
}

#[test]
fn empty_list_item_followed_by_blank_line_and_sibling_item() {
    assert_eq!(
        parse("-\n\n- second item\n"),
        vec![
            Event::Start(Tag::List(ListKind::Unordered)),
            Event::Start(Tag::Item),
            Event::End,
            Event::Start(Tag::Item),
            Event::Start(Tag::Paragraph),
            Event::Text("second item".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );

    assert_eq!(
        parse("1.\n\n1. second item\n"),
        vec![
            Event::Start(Tag::List(ListKind::Ordered(1))),
            Event::Start(Tag::Item),
            Event::End,
            Event::Start(Tag::Item),
            Event::Start(Tag::Paragraph),
            Event::Text("second item".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
}
