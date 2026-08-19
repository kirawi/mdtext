// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use std::fmt::Write;

use mdtext::{Options, Parser};

mod common;

use common::normalize_html;

const SPEC: &str = include_str!("../spec.txt");
const EXAMPLE_FENCE: &str = "````````````````````````````````";

#[derive(Debug)]
struct SpecExample {
    number: usize,
    markdown: String,
    html: String,
}

fn spec_examples() -> Vec<SpecExample> {
    let mut examples = Vec::new();
    let mut lines = SPEC.lines();

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
    for event in Parser::parse_str(markdown, Options::empty()) {
        writer.push_event(&event);
    }
    writer.into_string()
}

// Reference links are not supported and these excluded tests all require them.
const BY_DESIGN_EXCLUDED: &[usize] = &[
    23, 33, 194, 195, 196, 197, 198, 200, 202, 204, 205, 206, 207, 208, 209, 210, 212, 216, 217,
    218, 219, 220, 319, 529, 530, 531, 532, 533, 534, 535, 536, 537, 538, 539, 540, 541, 542, 543,
    544, 545, 546, 547, 551, 552, 555, 556, 557, 558, 559, 560, 561, 562, 563, 564, 565, 566, 567,
    568, 569, 570, 571, 572, 573, 575, 578, 579, 584, 585, 586, 587, 588, 589, 590, 591, 593, 594,
    595,
];

#[test]
fn commonmark_spec_examples_render_expected_html() {
    let examples = spec_examples();
    assert_eq!(
        examples.len(),
        655,
        "expected all CommonMark 0.31.2 examples"
    );

    let excluded: std::collections::HashSet<usize> = BY_DESIGN_EXCLUDED.iter().copied().collect();

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
        "{failure_count} of {} non-excluded CommonMark examples failed ({skipped_count} skipped by design).{failures}",
        655 - skipped_count,
    );
}
