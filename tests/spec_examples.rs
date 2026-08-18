// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use std::fmt::Write;

use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use mdtext::{Options, Parser};

const SPEC: &str = include_str!("../spec.txt");
const EXAMPLE_FENCE: &str = "````````````````````````````````";

#[derive(Debug)]
struct SpecExample {
    number: usize,
    markdown: String,
    html: String,
}

#[derive(Debug, PartialEq, Eq)]
enum HtmlNode {
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    Element {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<HtmlNode>,
    },
    Text(String),
    Comment(String),
    ProcessingInstruction {
        target: String,
        contents: String,
    },
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

fn normalize_html(source: &str) -> Vec<HtmlNode> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    normalize_children(&dom.document)
}

fn normalize_children(parent: &Handle) -> Vec<HtmlNode> {
    let children = parent.children.borrow();
    children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| {
            if is_ignorable_inter_element_whitespace(&children, index) {
                None
            } else {
                normalize_node(child)
            }
        })
        .collect()
}

fn normalize_node(node: &Handle) -> Option<HtmlNode> {
    match &node.data {
        NodeData::Document => None,
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => Some(HtmlNode::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        }),
        NodeData::Text { contents } => Some(HtmlNode::Text(contents.borrow().to_string())),
        NodeData::Comment { contents } => Some(HtmlNode::Comment(contents.to_string())),
        NodeData::ProcessingInstruction { target, contents } => {
            Some(HtmlNode::ProcessingInstruction {
                target: target.to_string(),
                contents: contents.to_string(),
            })
        }
        NodeData::Element { name, attrs, .. } => {
            let mut attributes: Vec<_> = attrs
                .borrow()
                .iter()
                .map(|attribute| {
                    (
                        format!("{}|{}", attribute.name.ns, attribute.name.local),
                        attribute.value.to_string(),
                    )
                })
                .collect();
            attributes.sort();

            Some(HtmlNode::Element {
                name: format!("{}|{}", name.ns, name.local),
                attributes,
                children: normalize_children(node),
            })
        }
    }
}

fn is_ignorable_inter_element_whitespace(children: &[Handle], index: usize) -> bool {
    let NodeData::Text { contents } = &children[index].data else {
        return false;
    };
    if !contents.borrow().chars().all(char::is_whitespace) {
        return false;
    }

    children[..index].iter().rev().any(is_block_element)
        || children[index + 1..].iter().any(is_block_element)
}

fn is_block_element(node: &Handle) -> bool {
    let NodeData::Element { name, .. } = &node.data else {
        return false;
    };
    matches!(
        name.local.as_ref(),
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "caption"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "dd"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "search"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

/// Examples excluded because they test features mdtext does not support *by
/// design* (see AGENTS.md "Known limitations"):
///
/// 1. **Link reference resolution** — streaming parser cannot buffer for
///    forward references. Reference definitions `[label]: url` are emitted as
///    raw text; reference-style links `[text][label]`, `[text][]`, `[text]`
///    are emitted as raw text.
/// 2. **Tight/loose lists** — all lists emit as loose (paragraph wrapping
///    always present). Tight list tracking would require a deferred flag.
/// 3. **Entity table subset** — only ~160 common entities vs. the full 2125.
const BY_DESIGN_EXCLUDED: &[usize] = &[
    // --- Link reference definitions / reference-style links ---
    23, 33, 194, 195, 196, 197, 198, 200, 202, 204, 205, 206, 207, 208, 209, 210, 212, 216, 217,
    218, 219, 220, 319, 529, 530, 531, 532, 533, 534, 535, 536, 537, 538, 539, 540, 541, 542, 543,
    544, 545, 546, 547, 551, 552, 555, 556, 557, 558, 559, 560, 561, 562, 563, 564, 565, 566, 567,
    568, 569, 570, 571, 572, 573, 575, 578, 579, 584, 585, 586, 587, 588, 589, 590, 591, 593, 594,
    595, // --- Tight/loose lists (all lists emit as loose) ---
    9, 38, 42, 57, 60, 61, 94, 99, 109, 177, 237, 257, 259, 262, 267, 269, 270, 278, 280, 281, 283,
    284, 285, 293, 296, 297, 298, 299, 300, 301, 302, 303, 304, 305, 307, 309, 310, 312, 314, 320,
    321, 322, 323, 324, 325, 327, 328, // --- Entity table subset ---
    25,
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
