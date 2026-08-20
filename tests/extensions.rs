// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use mdtext::{Alignment, Event, Options, Parser, Tag, filter_disallowed_html};

fn parse(markdown: &str, options: Options) -> Vec<Event<'_>> {
    Parser::parse_str(markdown, options)
}

#[test]
fn options_are_independent_and_default_mode_is_unchanged() {
    let source = "a | b\n--- | ---\n- [x] done\n~~gone~~ www.example.com $x$\n";
    let default: Vec<_> = Parser::parse_str(source, Options::empty());
    assert_eq!(default, parse(source, Options::empty()));
    assert!(!default.iter().any(|event| {
        matches!(
            event,
            Event::TaskListMarker(_)
                | Event::InlineMath(_)
                | Event::DisplayMath(_)
                | Event::Start(Tag::Table(_))
                | Event::Start(Tag::Strikethrough)
        )
    }));
    assert!(!Options::GFM.contains(Options::MATH_DOLLARS));
}

#[test]
fn gfm_minimizes_same_delimiter_emphasis_nesting() {
    assert_eq!(
        parse("****foo****", Options::empty()),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Strong),
            Event::Start(Tag::Strong),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("****foo****", Options::TABLES),
        parse("****foo****", Options::empty())
    );
    assert_eq!(
        parse("****foo****", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Strong),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("*****foo*****", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Emphasis),
            Event::Start(Tag::Strong),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );

    assert_eq!(
        parse("N****a*****b****c", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("N".into()),
            Event::Start(Tag::Strong),
            Event::Text("a".into()),
            Event::Text("*****".into()),
            Event::Text("b".into()),
            Event::End,
            Event::Text("c".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("***!!**LARGEST FISH**!!***", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Emphasis),
            Event::Start(Tag::Strong),
            Event::Text("!!".into()),
            Event::Text("LARGEST FISH".into()),
            Event::Text("!!".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn gfm_only_minimizes_direct_strong_nesting() {
    assert_eq!(
        parse("__**foo**__", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Strong),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("__***foo***__", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Strong),
            Event::Start(Tag::Emphasis),
            Event::Start(Tag::Strong),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("***__foo__***", Options::GFM),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Emphasis),
            Event::Start(Tag::Strong),
            Event::Text("foo".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn disabled_extensions_preserve_commonmark_interpretation() {
    let table = parse("a | b\n--- | ---\n", Options::empty());
    assert!(
        !table
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Table(_))))
    );

    let task = parse("- [x] done\n", Options::empty());
    let task_text: String = task
        .iter()
        .filter_map(|event| match event {
            Event::Text(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(task_text, "[x] done");
    assert!(!task.contains(&Event::TaskListMarker(true)));

    let inline = parse(
        "~~gone~~ www.example.com foo@example.com $x$ $`y`$",
        Options::empty(),
    );
    assert!(!inline.iter().any(|event| {
        matches!(
            event,
            Event::Start(Tag::Strikethrough | Tag::Link { .. })
                | Event::InlineMath(_)
                | Event::DisplayMath(_)
        )
    }));
    assert!(inline.contains(&Event::Code("y".into())));

    let raw = "<script>x</script>\n";
    assert_eq!(parse(raw, Options::TAGFILTER), parse(raw, Options::empty()));
}

#[test]
fn strikethrough_supports_one_and_two_tildes() {
    let events = parse("~one~ and ~~two~~ and ~~~three~~~", Options::STRIKETHROUGH);
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == Event::Start(Tag::Strikethrough))
            .count(),
        2
    );
    let literal_text: String = events
        .iter()
        .filter_map(|event| match event {
            Event::Text(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    assert!(literal_text.ends_with(" and ~~~three~~~"));
}

#[test]
fn gfm_punctuation_excludes_unicode_symbols() {
    fn struck_text(events: &[Event<'_>]) -> Option<String> {
        let start = events
            .iter()
            .position(|event| *event == Event::Start(Tag::Strikethrough))?;
        match (events.get(start + 1), events.get(start + 2)) {
            (Some(Event::Text(text)), Some(Event::End)) => Some(text.to_string()),
            _ => None,
        }
    }

    let source = "$latex r~×~B~>~C";
    let commonmark = parse(source, Options::STRIKETHROUGH);
    let gfm = parse(source, Options::STRIKETHROUGH | Options::GFM_DIALECT);

    assert_eq!(struck_text(&commonmark).as_deref(), Some("B"));
    assert_eq!(struck_text(&gfm).as_deref(), Some("×"));
}

#[test]
fn strikethrough_nests_with_other_inline_atoms() {
    let events = parse(
        "~~*em* [link](target) `code` <i>raw</i>~~",
        Options::STRIKETHROUGH,
    );
    assert!(events.contains(&Event::Start(Tag::Strikethrough)));
    assert!(events.contains(&Event::Start(Tag::Emphasis)));
    assert!(events.contains(&Event::Start(Tag::Link {
        url: "target".into(),
        title: None,
    })));
    assert!(events.contains(&Event::Code("code".into())));
    assert!(events.contains(&Event::Html("<i>".into())));
}

#[test]
fn link_labels_preserve_delimiter_flanking_around_opaque_atoms() {
    assert_eq!(
        parse("[~~`x`~~](u)", Options::STRIKETHROUGH),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Link {
                url: "u".into(),
                title: None,
            }),
            Event::Start(Tag::Strikethrough),
            Event::Start(Tag::CodeSpan),
            Event::Code("x".into()),
            Event::End,
            Event::End,
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("[*answer: $x$*](u)", Options::MATH_DOLLARS),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Link {
                url: "u".into(),
                title: None,
            }),
            Event::Start(Tag::Emphasis),
            Event::Text("answer: ".into()),
            Event::InlineMath("x".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
    let html = parse("[~~<i>x</i>~~](u)", Options::STRIKETHROUGH);
    assert!(
        html.windows(2).any(|events| {
            events == [Event::Start(Tag::Strikethrough), Event::Html("<i>".into())]
        })
    );
}

#[test]
fn math_atoms_are_opaque_and_fenced_math_is_display_math() {
    assert_eq!(
        parse("*$x_*`y`$* $$ z + *w* $$ $`a*b`$", Options::MATH),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Emphasis),
            Event::InlineMath("x_*`y`".into()),
            Event::End,
            Event::Text(" ".into()),
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath(" z + *w* ".into()),
            Event::End,
            Event::Text(" ".into()),
            Event::InlineMath("a*b".into()),
            Event::End,
        ]
    );

    assert_eq!(
        parse("```math\nx^2 + y^2\n```\n", Options::MATH_CODE),
        vec![
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("x^2 + y^2\n".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("```math\nx\n```\n", Options::empty()),
        vec![
            Event::Start(Tag::CodeBlock(Some("math".into()))),
            Event::Code("x\n".into()),
            Event::End,
        ]
    );
}

#[test]
fn escaped_math_code_closer_falls_back_to_a_code_span() {
    assert_eq!(
        parse(r"$`\sqrt{x^2 + y^2}\`$", Options::MATH),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("$".into()),
            Event::Start(Tag::CodeSpan),
            Event::Code(r"\sqrt{x^2 + y^2}\".into()),
            Event::End,
            Event::Text("$".into()),
            Event::End,
        ]
    );
}

#[test]
fn adjacent_math_code_closer_falls_through_to_dollar_math() {
    assert_eq!(
        parse("$``$ $`x`$", Options::MATH),
        vec![
            Event::Start(Tag::Paragraph),
            Event::InlineMath("``".into()),
            Event::Text(" ".into()),
            Event::InlineMath("x".into()),
            Event::End,
        ]
    );
}

#[test]
fn dollar_math_does_not_skip_an_invalid_first_closer() {
    assert_eq!(
        parse("$a$2 b$", Options::MATH_DOLLARS),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("$a".into()),
            Event::InlineMath("2 b".into()),
            Event::End,
        ]
    );
}

#[test]
fn dollar_constraints_leave_ambiguous_input_literal() {
    let events = parse(
        "$ x$ $x $ $x$2 $$$x$$$ \\$escaped$ $ok$",
        Options::MATH_DOLLARS,
    );
    assert!(events.contains(&Event::InlineMath("ok".into())));
    assert!(!events.iter().any(|event| {
        matches!(event, Event::InlineMath(source) if *source == " x" || *source == "x ")
    }));
}

#[test]
fn math_respects_escape_parity_leaf_boundaries_and_markdown_precedence() {
    let escaped = parse("\\$no$ \\\\$yes$", Options::MATH_DOLLARS);
    assert!(!escaped.contains(&Event::InlineMath("no".into())));
    assert!(escaped.contains(&Event::InlineMath("yes".into())));

    let interactions = parse(
        "[$x &amp; * y$](target) and `$protected$` and $line one\nline two$\n",
        Options::MATH_DOLLARS,
    );
    assert!(interactions.contains(&Event::Start(Tag::Link {
        url: "target".into(),
        title: None,
    })));
    assert!(interactions.contains(&Event::InlineMath("x &amp; * y".into())));
    assert!(interactions.contains(&Event::Code("$protected$".into())));
    assert!(interactions.contains(&Event::InlineMath("line one\nline two".into())));

    let split_leaf = parse("$cannot\n\ncross$", Options::MATH_DOLLARS);
    assert!(
        !split_leaf
            .iter()
            .any(|event| matches!(event, Event::InlineMath(_) | Event::DisplayMath(_)))
    );

    let disabled = parse("$`x`$", Options::empty());
    assert!(disabled.contains(&Event::Code("x".into())));
    assert!(
        !disabled
            .iter()
            .any(|event| matches!(event, Event::InlineMath(_) | Event::DisplayMath(_)))
    );
}

#[test]
fn extended_autolinks_trim_and_construct_destinations() {
    let events = parse(
        "www.example.com, https://example.com/a_(b). foo@example.com mailto:a@b.com xmpp:a@b.com/r",
        Options::EXTENDED_AUTOLINKS,
    );
    assert!(events.contains(&Event::Start(Tag::Link {
        url: "http://www.example.com".into(),
        title: None,
    })));
    assert!(events.contains(&Event::Start(Tag::Link {
        url: "https://example.com/a_(b)".into(),
        title: None,
    })));
    assert!(events.contains(&Event::Start(Tag::Link {
        url: "mailto:foo@example.com".into(),
        title: None,
    })));
    assert!(events.contains(&Event::Start(Tag::Link {
        url: "mailto:a@b.com".into(),
        title: None,
    })));
    assert!(events.contains(&Event::Start(Tag::Link {
        url: "xmpp:a@b.com/r".into(),
        title: None,
    })));
    assert!(
        parse("www.example.com1.", Options::EXTENDED_AUTOLINKS).contains(&Event::Start(
            Tag::Link {
                url: "http://www.example.com1".into(),
                title: None,
            }
        ))
    );

    let protected = parse(
        "`www.example.com` [www.example.com](target) [www.example.com]",
        Options::EXTENDED_AUTOLINKS,
    );
    assert_eq!(
        protected
            .iter()
            .filter(|event| matches!(event, Event::Start(Tag::Link { .. })))
            .count(),
        1
    );
    let ftp_result = parse("ftp://example.com", Options::EXTENDED_AUTOLINKS);
    assert!(link_targets(&ftp_result).is_empty());
}

#[test]
fn task_markers_only_apply_to_the_first_direct_paragraph() {
    let events = parse(
        "- [x] done\n-\n  [ ] later\n- > [x] nested quote text\n- first\n\n  [x] second\n",
        Options::TASK_LISTS,
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                Event::TaskListMarker(checked) => Some(*checked),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![true, false]
    );
    assert!(
        !parse(
            "- # first block\n\n  [x] later paragraph\n",
            Options::TASK_LISTS,
        )
        .iter()
        .any(|event| matches!(event, Event::TaskListMarker(_)))
    );
}

#[test]
fn tables_emit_alignment_and_normalized_rows() {
    let events = parse(
        "intro\na | b\\|c | d\n:--- | ---: | :---:\n1 | 2\n3\n4 | 5 | kept | ignored\n\nafter\n",
        Options::TABLES,
    );
    assert_eq!(
        &events[..4],
        &[
            Event::Start(Tag::Paragraph),
            Event::Text("intro".into()),
            Event::End,
            Event::Start(Tag::Table(vec![
                Alignment::Left,
                Alignment::Right,
                Alignment::Center,
            ])),
        ]
    );
    assert!(events.contains(&Event::Text("b".into())));
    assert!(events.contains(&Event::Text("|c".into())));
    assert!(!events.contains(&Event::Text("ignored".into())));
    assert!(events.contains(&Event::Text("kept".into())));
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableCell))
            .count(),
        12
    );
    assert!(
        events.windows(2).any(|window| {
            window == [Event::Start(Tag::Paragraph), Event::Text("after".into())]
        })
    );
}

#[test]
fn invalid_table_delimiters_fall_back_without_losing_text() {
    let events = parse("a | b\n---\n", Options::TABLES);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Table(_))))
    );
    assert!(events.contains(&Event::Text("a | b".into())));
    assert!(events.contains(&Event::Start(Tag::Heading(2))));

    // A colon-only cell has both alignment markers but no hyphen run.
    let events = parse("a | b\n: | ---\n", Options::TABLES);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Table(_))))
    );
}

#[test]
fn table_delimiters_allow_at_most_three_columns_of_indent() {
    assert!(
        parse("a | b\n   --- | ---\n", Options::TABLES)
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Table(_))))
    );
    for source in ["a | b\n    --- | ---\n", "a | b\n\t--- | ---\n"] {
        assert!(
            !parse(source, Options::TABLES)
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Table(_))))
        );
    }
}

#[test]
fn tag_filter_preserves_parser_html_and_filters_renderer_payloads() {
    let source = "<script>x</script><span>ok</span><textarea name=x>";
    assert_eq!(
        filter_disallowed_html(source),
        "&lt;script>x&lt;/script><span>ok</span>&lt;textarea name=x>"
    );
    let parsed = parse(source, Options::TAGFILTER);
    assert!(
        parsed
            .iter()
            .any(|event| matches!(event, Event::Html(html) if html.contains("<script>")))
    );
    assert_eq!(
        filter_disallowed_html("<strong> <title> <style> <em> <xmp> <XMP> <scripted> <script/x>"),
        "<strong> &lt;title> &lt;style> <em> &lt;xmp> &lt;XMP> <scripted> <script/x>"
    );
    let example_657 = "<strong> <title> <style> <em>\n\n<blockquote>\n  <xmp> is disallowed.  <XMP> is also disallowed.\n</blockquote>";
    assert_eq!(
        filter_disallowed_html(example_657),
        "<strong> &lt;title> &lt;style> <em>\n\n<blockquote>\n  &lt;xmp> is disallowed.  &lt;XMP> is also disallowed.\n</blockquote>"
    );
}

fn link_targets<'a, 'b>(events: &'b [Event<'a>]) -> Vec<&'b str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Start(Tag::Link { url, .. }) => Some(url.as_ref()),
            _ => None,
        })
        .collect()
}

#[test]
fn pinned_gfm_table_examples_198_through_205() {
    let table = parse(
        "| foo | bar |\n| --- | --- |\n| baz | bim |\n",
        Options::TABLES,
    );
    assert_eq!(
        table
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableCell))
            .count(),
        4
    );

    let alignment = parse(
        "| abc | defghi |\n:-: | -----------:\nbar | baz\n",
        Options::TABLES,
    );
    assert!(alignment.contains(&Event::Start(Tag::Table(vec![
        Alignment::Center,
        Alignment::Right,
    ]))));

    let escaped = parse(
        "| f\\|oo  |\n| ------ |\n| b `\\|` az |\n| b **\\|** im |\n",
        Options::TABLES,
    );
    assert!(escaped.contains(&Event::Text("f".into())));
    assert!(escaped.contains(&Event::Text("|oo".into())));
    assert!(escaped.contains(&Event::Code("|".into())));
    assert!(escaped.contains(&Event::Start(Tag::Strong)));

    let interrupted = parse(
        "| abc | def |\n| --- | --- |\n| bar | baz |\n> bar\n",
        Options::TABLES,
    );
    assert!(interrupted.contains(&Event::Start(Tag::Quote)));

    let short_row = parse(
        "| abc | def |\n| --- | --- |\n| bar | baz |\nbar\n\nbar\n",
        Options::TABLES,
    );
    assert_eq!(
        short_row
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableCell))
            .count(),
        6
    );

    let mismatch = parse("| abc | def |\n| --- |\n| bar |\n", Options::TABLES);
    assert!(
        !mismatch
            .iter()
            .any(|event| matches!(event, Event::Start(Tag::Table(_))))
    );

    let variable = parse(
        "| abc | def |\n| --- | --- |\n| bar |\n| bar | baz | boo |\n",
        Options::TABLES,
    );
    assert_eq!(
        variable
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableCell))
            .count(),
        6
    );
    assert!(!variable.contains(&Event::Text("boo".into())));

    let no_body = parse("| abc | def |\n| --- | --- |\n", Options::TABLES);
    assert!(!no_body.contains(&Event::Start(Tag::TableBody)));

    let lone_framing_pipe = parse("| abc | def |\n| --- | --- |\n|\n", Options::TABLES);
    assert_eq!(
        lone_framing_pipe
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableRow))
            .count(),
        1
    );
    assert!(lone_framing_pipe.contains(&Event::Text("|".into())));
}

#[test]
fn pinned_task_and_strikethrough_examples() {
    let flat_tasks = parse("- [ ] foo\n- [x] bar\n", Options::TASK_LISTS);
    assert_eq!(
        flat_tasks
            .iter()
            .filter_map(|event| match event {
                Event::TaskListMarker(checked) => Some(*checked),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![false, true]
    );

    let tasks = parse(
        "- [x] foo\n  - [ ] bar\n  - [x] baz\n- [ ] bim\n",
        Options::TASK_LISTS,
    );
    assert_eq!(
        tasks
            .iter()
            .filter_map(|event| match event {
                Event::TaskListMarker(checked) => Some(*checked),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![true, false, true, false]
    );

    let strike = parse(
        "~~Hi~~ Hello, ~there~ world!\n\nThis ~~has a\n\nnew paragraph~~.\n\nThis will ~~~not~~~ strike.\n",
        Options::STRIKETHROUGH,
    );
    assert_eq!(
        strike
            .iter()
            .filter(|event| **event == Event::Start(Tag::Strikethrough))
            .count(),
        2
    );
}

#[test]
fn pinned_extended_autolink_examples_622_through_635() {
    let web = parse(
        "www.commonmark.org\n\nVisit www.commonmark.org/help for more information.\n\nVisit www.commonmark.org.\n\nVisit www.commonmark.org/a.b.\n\nwww.google.com/search?q=Markup+(business)\n\nwww.google.com/search?q=Markup+(business)))\n\n(www.google.com/search?q=Markup+(business))\n\n(www.google.com/search?q=Markup+(business)\n\nwww.google.com/search?q=(business))+ok\n\nwww.google.com/search?q=commonmark&hl=en\n\nwww.google.com/search?q=commonmark&hl;\n\nwww.commonmark.org/he<lp\n\nhttp://commonmark.org\n\n(Visit https://encrypted.google.com/search?q=Markup+(business))\n",
        Options::EXTENDED_AUTOLINKS,
    );
    let targets = link_targets(&web);
    assert!(targets.contains(&"http://www.commonmark.org"));
    assert!(targets.contains(&"http://www.commonmark.org/help"));
    assert!(targets.contains(&"http://www.commonmark.org/a.b"));
    assert!(targets.contains(&"http://www.google.com/search?q=Markup+(business)"));
    assert!(targets.contains(&"http://www.google.com/search?q=(business))+ok"));
    assert!(targets.contains(&"http://www.google.com/search?q=commonmark&hl=en"));
    assert!(targets.contains(&"http://www.google.com/search?q=commonmark"));
    assert!(targets.contains(&"http://www.commonmark.org/he"));
    assert!(targets.contains(&"http://commonmark.org"));
    assert!(targets.contains(&"https://encrypted.google.com/search?q=Markup+(business)"));

    let protocols = parse(
        "foo@bar.baz\n\nhello@mail+xyz.example isn't valid, but hello+xyz@mail.example is.\n\na.b-c_d@a.b\n\na.b-c_d@a.b.\n\na.b-c_d@a.b-\n\na.b-c_d@a.b_\n\nmailto:foo@bar.baz\n\nmailto:a.b-c_d@a.b\n\nmailto:a.b-c_d@a.b.\n\nmailto:a.b-c_d@a.b/\n\nmailto:a.b-c_d@a.b-\n\nmailto:a.b-c_d@a.b_\n\nxmpp:foo@bar.baz\n\nxmpp:foo@bar.baz.\n\nxmpp:foo@bar.baz/txt\n\nxmpp:foo@bar.baz/txt@bin\n\nxmpp:foo@bar.baz/txt@bin.com\n\nxmpp:foo@bar.baz/txt/bin\n",
        Options::EXTENDED_AUTOLINKS,
    );
    let targets = link_targets(&protocols);
    assert_eq!(
        targets,
        vec![
            "mailto:foo@bar.baz",
            "mailto:hello+xyz@mail.example",
            "mailto:a.b-c_d@a.b",
            "mailto:a.b-c_d@a.b",
            "mailto:foo@bar.baz",
            "mailto:a.b-c_d@a.b",
            "mailto:a.b-c_d@a.b",
            "mailto:a.b-c_d@a.b",
            "xmpp:foo@bar.baz",
            "xmpp:foo@bar.baz",
            "xmpp:foo@bar.baz/txt",
            "xmpp:foo@bar.baz/txt@bin",
            "xmpp:foo@bar.baz/txt@bin.com",
            "xmpp:foo@bar.baz/txt",
        ]
    );
}

#[test]
fn extension_output_is_independent_of_bufread_chunk_boundaries() {
    let formula = "x_i + y_i ".repeat(4_096);
    let source = format!(
        "> | a | b |\r\n> | :- | -: |\r\n> | $x$ | ~~www.example.com~~ |\r\n\r\n- [x] $`a*b`$\r\n\r\n${formula}$\r\n"
    );
    let options = Options::GFM | Options::MATH;
    let expected = parse(&source, options);
    for chunk in 1..=11 {
        let mut parser = Parser::with_options(options);
        let mut all_events: Vec<Event> = Vec::new();
        let mut pos = 0;
        while pos < source.len() {
            let mut end = (pos + chunk).min(source.len());
            let consumed;
            loop {
                let (events, cons) = parser.feed_chunk(&source[pos..end]);
                all_events.extend(events);
                if cons > 0 || end == source.len() {
                    consumed = cons;
                    break;
                }
                end = (end + chunk).min(source.len());
            }
            if consumed > 0 {
                pos += consumed;
            } else {
                all_events.extend(parser.finish(&source[pos..end]));
                break;
            }
        }
        assert_eq!(all_events, expected);
    }
}

#[test]
fn tables_stream_many_rows_and_cap_synthesized_cells() {
    let mut many_rows = String::from("a | b\n--- | ---\n");
    for row in 0..2_000 {
        use std::fmt::Write;
        let _ = writeln!(many_rows, "{row} | value");
    }
    let body_rows = Parser::parse_str(&many_rows, Options::TABLES)
        .iter()
        .filter(|event| **event == Event::Start(Tag::TableRow))
        .count();
    assert_eq!(body_rows, 2_001);

    let columns = 257;
    let mut wide = std::iter::repeat_n("h", columns)
        .collect::<Vec<_>>()
        .join("|");
    wide.push('\n');
    wide.push_str(
        &std::iter::repeat_n("---", columns)
            .collect::<Vec<_>>()
            .join("|"),
    );
    wide.push('\n');

    let body_rows_in_table = 2_048;
    for _ in 0..(body_rows_in_table + 1) {
        wide.push_str("x\n");
    }
    let events = parse(&wide, Options::TABLES);
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableCell))
            .count(),
        columns * (body_rows_in_table + 1) // header + body rows
    );
    assert!(
        events
            .windows(2)
            .any(|window| { window == [Event::Start(Tag::Paragraph), Event::Text("x".into())] })
    );
}

#[test]
fn malformed_delimiter_heavy_inlines_remain_bounded() {
    let mut source = "$ ".repeat(20_000);
    source.push_str(&"$`unterminated ".repeat(20_000));
    source.push_str(&"~ ".repeat(20_000));
    source.push_str(&"ordinary".repeat(10_000));
    let events = parse(
        &source,
        Options::MATH | Options::STRIKETHROUGH | Options::EXTENDED_AUTOLINKS,
    );
    assert_eq!(events.first(), Some(&Event::Start(Tag::Paragraph)));
    assert_eq!(events.last(), Some(&Event::End));
}

#[test]
fn latex_delimiters_produce_inline_and_display_math() {
    assert_eq!(
        parse(r"\(x^2\) and \[y = mx + b\]", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::Paragraph),
            Event::InlineMath("x^2".into()),
            Event::Text(" and ".into()),
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("y = mx + b".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn latex_delimiters_span_multiple_lines_in_paragraph() {
    assert_eq!(
        parse("\\(line one\nline two\\)", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::Paragraph),
            Event::InlineMath("line one\nline two".into()),
            Event::End,
        ]
    );
    assert_eq!(
        parse("\\[a\nb\nc\\]", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("a\n".into()),
            Event::DisplayMath("b\n".into()),
            Event::DisplayMath("c".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn math_scanners_skip_container_prefix_gaps_and_commit_their_span() {
    let expected = vec![
        Event::Start(Tag::Quote),
        Event::Start(Tag::Paragraph),
        Event::InlineMath("a\nb".into()),
        Event::Text(" x".into()),
        Event::SoftBreak,
        Event::Text("y".into()),
        Event::End,
        Event::End,
    ];
    assert_eq!(
        parse("> \\(a\n> b\\) x\n> y", Options::MATH_LATEX),
        expected
    );
    assert_eq!(parse("> $a\n> b$ x\n> y", Options::MATH_DOLLARS), expected);
}

#[test]
fn delimiter_only_display_math_is_an_opaque_block() {
    let latex = concat!(
        "\\[\n",
        "\\delta(q_{\\mathrm{seek}}(C,e),\\epsilon,[O,b,S])\n",
        "=\n",
        "(q_{\\mathrm{seek}}(C,e),\\epsilon).\n",
        "\\]\n",
    );
    assert_eq!(
        parse(latex, Options::MATH_LATEX),
        vec![
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath(
                "\\delta(q_{\\mathrm{seek}}(C,e),\\epsilon,[O,b,S])\n=\n(q_{\\mathrm{seek}}(C,e),\\epsilon).\n".into(),
            ),
            Event::End,
        ]
    );

    assert_eq!(
        parse("$$\nx\n=\ny\n$$\n", Options::MATH_DOLLARS),
        vec![
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("x\n=\ny\n".into()),
            Event::End,
        ]
    );
}

#[test]
fn display_math_blocks_shield_markdown_block_starts() {
    let body = concat!(
        "=\n",
        "---\n",
        "# heading-like TeX\n",
        "> quote-like TeX\n",
        "- list-like TeX\n",
        "```not-a-fence\n",
        "<script>not HTML</script>\n",
        "a | b\n",
        "--- | ---\n",
    );
    let source = format!("\\[\n{body}\\]\n");
    let parse = parse(&source, Options::GFM | Options::MATH_LATEX);
    let expected = vec![
        Event::Start(Tag::DisplayMath),
        Event::DisplayMath(body.into()),
        Event::End,
    ];
    assert_eq!(parse, expected);
}

#[test]
fn display_math_blocks_interrupt_paragraphs_and_tables() {
    assert_eq!(
        parse("before\n\\[\nx\n\\]\nafter\n", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Text("before".into()),
            Event::End,
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("x\n".into()),
            Event::End,
            Event::Start(Tag::Paragraph),
            Event::Text("after".into()),
            Event::End,
        ]
    );

    let table = parse(
        "a | b\n--- | ---\n1 | 2\n\\[\nx\n\\]\n",
        Options::TABLES | Options::MATH_LATEX,
    );
    assert!(table.contains(&Event::DisplayMath("x\n".into())));
    assert_eq!(
        table
            .iter()
            .filter(|event| **event == Event::Start(Tag::TableRow))
            .count(),
        2
    );
}

#[test]
fn display_math_blocks_respect_containers_and_eof() {
    assert_eq!(
        parse("> \\[\n> x\n> =\n> y\n> \\]\n", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::Quote),
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("x\n".into()),
            Event::DisplayMath("=\n".into()),
            Event::DisplayMath("y\n".into()),
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("- \\[\n  x\n  =\n  y\n  \\]\n", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::List(mdtext::ListKind::Unordered)),
            Event::Start(Tag::Item),
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("x\n".into()),
            Event::DisplayMath("=\n".into()),
            Event::DisplayMath("y\n".into()),
            Event::End,
            Event::End,
            Event::End,
        ]
    );
    assert_eq!(
        parse("\\[\nunterminated", Options::MATH_LATEX),
        vec![
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("unterminated".into()),
            Event::End,
        ]
    );
}

#[test]
fn disabled_display_math_blocks_keep_commonmark_precedence() {
    let events = parse("\\[\nx\n=\ny\n\\]\n", Options::empty());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::DisplayMath(_)))
    );
    assert!(events.contains(&Event::Start(Tag::Heading(1))));
}

#[test]
fn latex_delimiters_are_opaque_to_emphasis() {
    let events = parse(r"\(*x*\) and \[*y*\]", Options::MATH_LATEX);
    assert_eq!(
        events,
        vec![
            Event::Start(Tag::Paragraph),
            Event::InlineMath("*x*".into()),
            Event::Text(" and ".into()),
            Event::Start(Tag::DisplayMath),
            Event::DisplayMath("*y*".into()),
            Event::End,
            Event::End,
        ]
    );
}

#[test]
fn latex_delimiters_disabled_by_default() {
    let events = parse(r"\(x\) \[y\]", Options::empty());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::InlineMath(_) | Event::DisplayMath(_)))
    );
    // Without the option, \( and \[ are escaped punctuation → literal ( and [.
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Event::Text(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "(x) [y]");
}

#[test]
fn latex_delimiters_do_not_cross_paragraph_boundaries() {
    let events = parse("\\(cannot\n\ncross\\)", Options::MATH_LATEX);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::InlineMath(_) | Event::DisplayMath(_)))
    );
}

#[test]
fn latex_delimiters_work_inside_link_text_and_headings() {
    // Inside a heading:
    let heading = parse("# \\(x^2\\)", Options::MATH_LATEX);
    assert!(heading.contains(&Event::InlineMath("x^2".into())));

    // Inside a table cell:
    let table = parse(
        r"a | b
--- | ---
\(x\) | \[y\]",
        Options::TABLES | Options::MATH_LATEX,
    );
    assert!(table.contains(&Event::InlineMath("x".into())));
    assert!(table.contains(&Event::DisplayMath("y".into())));
}

#[test]
fn latex_delimiters_unmatched_fall_back_to_escape() {
    // \( with no matching \) → treated as escaped (
    let events = parse(r"\(unmatched", Options::MATH_LATEX);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::InlineMath(_)))
    );
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Event::Text(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "(unmatched");
}

#[test]
fn latex_and_dollar_math_coexist() {
    let events = parse(
        r"$a$ \(b\) $$c$$ \[d\]",
        Options::MATH_DOLLARS | Options::MATH_LATEX,
    );
    assert!(events.contains(&Event::InlineMath("a".into())));
    assert!(events.contains(&Event::InlineMath("b".into())));
    assert!(events.contains(&Event::DisplayMath("c".into())));
    assert!(events.contains(&Event::DisplayMath("d".into())));
}
