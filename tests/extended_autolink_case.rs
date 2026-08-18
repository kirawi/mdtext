// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use mdtext::{Event, Options, Parser, Tag};

fn parse(markdown: &str) -> Vec<Event<'_>> {
    Parser::parse_str(markdown, Options::EXTENDED_AUTOLINKS)
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

/// cmark-gfm matches the `http://` scheme case-insensitively, so every case
/// variant of the scheme is an extended URL autolink whose destination is the
/// visible text unchanged.
#[test]
fn http_scheme_is_case_insensitive() {
    for source in [
        "http://example.com",
        "HTTP://example.com",
        "HtTp://example.com",
        "hTtP://example.com",
    ] {
        let events = parse(source);
        assert_eq!(
            link_targets(&events),
            vec![source],
            "source: {source:?}, events: {events:?}"
        );
    }
}

/// The same case-insensitive scheme matching applies to `https://`.
#[test]
fn https_scheme_is_case_insensitive() {
    for source in [
        "https://example.com",
        "HTTPS://example.com",
        "HtTpS://example.com",
        "hTtPs://example.com",
    ] {
        let events = parse(source);
        assert_eq!(
            link_targets(&events),
            vec![source],
            "source: {source:?}, events: {events:?}"
        );
    }
}

/// cmark-gfm only recognises the literal lowercase `www.` prefix. Uppercase
/// or mixed-case variants are left as plain text.
#[test]
fn www_prefix_is_case_sensitive() {
    assert_eq!(
        parse("www.example.com"),
        vec![
            Event::Start(Tag::Paragraph),
            Event::Start(Tag::Link {
                url: "http://www.example.com".into(),
                title: None,
            }),
            Event::Text("www.example.com".into()),
            Event::End,
            Event::End,
        ]
    );

    for source in [
        "WWW.example.com",
        "WwW.example.com",
        "wWw.example.com",
        "Www.example.com",
        "wwW.example.com",
    ] {
        let events = parse(source);
        assert!(
            link_targets(&events).is_empty(),
            "source: {source:?} unexpectedly autolinked, events: {events:?}"
        );
    }
}

/// Only the `www.` prefix is case-sensitive; the domain itself may use any
/// case, just like the domain following an `http://` scheme.
#[test]
fn www_domain_case_does_not_matter() {
    assert_eq!(
        link_targets(&parse("www.EXAMPLE.com")),
        vec!["http://www.EXAMPLE.com"]
    );
    assert_eq!(
        link_targets(&parse("http://EXAMPLE.com")),
        vec!["http://EXAMPLE.com"]
    );
}
