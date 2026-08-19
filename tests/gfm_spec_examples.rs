// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use std::fmt::Write;
use std::hash::{Hash, Hasher};

use mdtext::{Options, Parser};

mod common;

use common::normalize_html;

const SPEC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/third_party/gfm-spec.txt"
));
const COMMONMARK_SPEC: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/third_party/spec.txt"));
const EXAMPLE_FENCE: &str = "````````````````````````````````";

#[derive(Debug)]
struct SpecExample {
    number: usize,
    markdown: String,
    html: String,
}

impl PartialEq for SpecExample {
    fn eq(&self, other: &Self) -> bool {
        self.markdown == other.markdown
    }
}

impl Eq for SpecExample {}

impl Hash for SpecExample {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.markdown.hash(state);
    }
}

fn spec_examples(spec: &str) -> Vec<SpecExample> {
    let mut examples = Vec::new();
    let mut lines = spec.lines();

    while let Some(line) = lines.next() {
        if line != format!("{EXAMPLE_FENCE} example") {
            continue;
        }

        let mut markdown = String::new();
        loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("example {} has no HTML separator", examples.len() + 1));
            if line == "." {
                break;
            }
            markdown.push_str(line);
            markdown.push('\n');
        }

        let mut expected_html = String::new();
        loop {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("example {} has no closing fence", examples.len() + 1));
            if line == EXAMPLE_FENCE {
                break;
            }
            expected_html.push_str(line);
            expected_html.push('\n');
        }

        examples.push(SpecExample {
            number: examples.len() + 1,
            markdown: markdown.replace('→', "\t"),
            html: expected_html.replace('→', "\t"),
        });
    }

    examples
}

fn render(markdown: &str) -> String {
    let mut writer = mdtext::html::HtmlWriter::new();
    for event in Parser::parse_str(markdown, Options::GFM) {
        writer.push_event(&event);
    }
    writer.into_string()
}

// - Duplication of commonmark with some differences
// - Purely malformed expected HTML (e.g. NBSP instead of ASCII space)
const BY_DESIGN_EXCLUDED: &[usize] = &[333, 353, 503, 531, 534, 536, 604, 607];

#[test]
fn gfm_examples() {
    let commonmark_examples: std::collections::HashSet<_> =
        spec_examples(COMMONMARK_SPEC).into_iter().collect();
    let examples: Vec<_> = spec_examples(SPEC)
        .into_iter()
        .filter(|example| !commonmark_examples.contains(example))
        .collect();
    let example_count = examples.len();
    let excluded: std::collections::HashSet<_> = BY_DESIGN_EXCLUDED.iter().copied().collect();

    let mut failure_count = 0;
    let mut skipped_count = 0;
    let mut failures = String::new();

    for example in examples {
        if excluded.contains(&example.number) {
            skipped_count += 1;
            continue;
        }
        let actual = render(&example.markdown);
        if normalize_html(&actual) == normalize_html(&example.html) {
            continue;
        }

        failure_count += 1;
        if failure_count <= 200 {
            let _ = writeln!(
                failures,
                "\nExample {}\nMarkdown:\n{}\nExpected HTML:\n{}\nActual HTML:\n{}",
                example.number, example.markdown, example.html, actual
            );
        }
    }

    assert_eq!(
        failure_count,
        0,
        "{failure_count} of {} non-excluded GFM-only examples failed ({skipped_count} skipped by design).{failures}",
        example_count - skipped_count,
    );
}
