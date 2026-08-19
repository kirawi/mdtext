// WARNING: All code here was written by LLMs based on bugs encountered while writing the library.
// Pending human review.

use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::collections::VecDeque;

#[derive(Debug, PartialEq, Eq)]
pub enum HtmlNode {
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

pub fn normalize_html(source: &str) -> Vec<HtmlNode> {
    let dom = parse_document(RcDom::default(), Default::default()).one(source);
    normalize_children(&dom.document)
}

fn normalize_children(parent: &Handle) -> Vec<HtmlNode> {
    let children = parent.children.borrow();
    let parent_is_list_item = matches!(
        &parent.data,
        NodeData::Element { name, .. } if name.local.as_ref() == "li"
    );
    let mut normalized: Vec<_> = children
        .iter()
        .enumerate()
        .flat_map(|(index, child)| {
            if is_ignorable_inter_element_whitespace(&children, index) {
                Vec::new()
            } else if parent_is_list_item && is_element(child, "p") {
                // Tight-list renderers omit paragraph tags directly inside
                // list items, while loose-list renderers retain them. Flatten
                // those wrappers so both representations normalize equally.
                normalize_children(child)
            } else {
                normalize_node(child).into_iter().collect()
            }
        })
        .collect();

    if parent_is_list_item {
        for index in 0..normalized.len() {
            let follows_block = index > 0 && is_normalized_block_element(&normalized[index - 1]);
            let precedes_block = normalized
                .get(index + 1)
                .is_some_and(is_normalized_block_element);
            if let HtmlNode::Text(text) = &mut normalized[index] {
                if follows_block {
                    *text = text.trim_start().to_owned();
                }
                if precedes_block {
                    text.truncate(text.trim_end().len());
                }
            }
        }
    }

    normalized
}

fn is_element(node: &Handle, expected: &str) -> bool {
    matches!(
        &node.data,
        NodeData::Element { name, .. } if name.local.as_ref() == expected
    )
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
            let is_table_cell = matches!(name.local.as_ref(), "th" | "td");
            let mut attributes: Vec<_> = attrs
                .borrow()
                .iter()
                .map(|attribute| {
                    let local = attribute.name.local.as_ref();
                    let value = attribute.value.to_string();
                    if is_table_cell {
                        if local == "align" {
                            return ("|align".to_owned(), value.to_ascii_lowercase());
                        }
                        if local == "style"
                            && let Some(alignment) = simple_text_alignment(&value)
                        {
                            return ("|align".to_owned(), alignment.to_owned());
                        }
                    }
                    (
                        format!("{}|{}", attribute.name.ns, attribute.name.local),
                        value,
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

/// Canonicalize the renderer's single-declaration table alignment style to
/// the legacy `align` attribute used by cmark-gfm. More complex style values
/// remain distinct so normalization does not hide unrelated CSS differences.
fn simple_text_alignment(style: &str) -> Option<&'static str> {
    let declaration = style.trim().strip_suffix(';').unwrap_or(style.trim());
    let (property, value) = declaration.split_once(':')?;
    if !property.trim().eq_ignore_ascii_case("text-align") {
        return None;
    }
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Some("left"),
        "center" => Some("center"),
        "right" => Some("right"),
        _ => None,
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
    is_block_name(name.local.as_ref())
}

fn is_normalized_block_element(node: &HtmlNode) -> bool {
    let HtmlNode::Element { name, .. } = node else {
        return false;
    };
    is_block_name(name.rsplit('|').next().unwrap_or(name))
}

fn is_block_name(name: &str) -> bool {
    matches!(
        name,
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

// ---------------------------------------------------------------------------
// Diff helpers
// ---------------------------------------------------------------------------

/// Shorten a node's debug string for compact display.
fn node_summary(node: &HtmlNode) -> String {
    match node {
        HtmlNode::Element {
            name,
            attributes,
            children,
        } => {
            let tag = name.rsplit('|').next().unwrap_or(name);
            let attrs: Vec<String> = attributes
                .iter()
                .map(|(k, v)| {
                    let key = k.rsplit('|').next().unwrap_or(k);
                    format!("{key}=\"{v}\"")
                })
                .collect();
            let text = descendant_text_preview(node);
            let text = if text.is_empty() {
                String::new()
            } else {
                format!(" text={text:?}")
            };
            if attrs.is_empty() {
                format!("<{tag}> [{} children]{text}", children.len())
            } else {
                format!(
                    "<{tag} {}> [{} children]{text}",
                    attrs.join(" "),
                    children.len()
                )
            }
        }
        HtmlNode::Text(t) => {
            let truncated = truncate_chars(t, 80);
            format!("text({truncated:?})")
        }
        HtmlNode::Comment(c) => format!("comment({c:?})"),
        HtmlNode::Doctype { name, .. } => format!("doctype({name:?})"),
        HtmlNode::ProcessingInstruction { target, .. } => format!("pi({target:?})"),
    }
}

fn descendant_text_preview(node: &HtmlNode) -> String {
    fn append(node: &HtmlNode, output: &mut String) {
        if output.chars().count() >= 80 {
            return;
        }
        match node {
            HtmlNode::Text(text) => output.extend(text.chars().take(80 - output.chars().count())),
            HtmlNode::Element { children, .. } => {
                for child in children {
                    append(child, output);
                }
            }
            _ => {}
        }
    }

    let mut output = String::new();
    append(node, &mut output);
    if output.chars().count() == 80 {
        output.push('…');
    }
    output
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        truncated.push('…');
    }
    truncated
}

fn useful_prefix_node(node: &HtmlNode) -> bool {
    !matches!(node, HtmlNode::Text(text) if text.trim().is_empty())
}

/// Walk two node trees in parallel and return a human-readable description
/// of the first divergence (path + side-by-side context), or `None` if they
/// are equal.
pub fn diff_trees(ours: &[HtmlNode], theirs: &[HtmlNode]) -> Option<String> {
    let mut shared_prefix = VecDeque::with_capacity(3);
    diff_at(ours, theirs, &Vec::new(), &mut shared_prefix)
        .map(|path| format_diff(ours, theirs, &path, &shared_prefix))
}

/// A path of child indices leading to the first divergence.
type Path = Vec<usize>;

fn diff_at(
    ours: &[HtmlNode],
    theirs: &[HtmlNode],
    path: &Path,
    shared_prefix: &mut VecDeque<(Path, String)>,
) -> Option<Path> {
    // Compare element by element.
    for i in 0..ours.len().max(theirs.len()) {
        let mut child_path = path.clone();
        child_path.push(i);
        match (ours.get(i), theirs.get(i)) {
            (None, Some(_)) | (Some(_), None) => return Some(child_path),
            (Some(a), Some(b)) => {
                if a == b {
                    if useful_prefix_node(a) {
                        if shared_prefix.len() == 3 {
                            shared_prefix.pop_front();
                        }
                        shared_prefix.push_back((child_path, node_summary(a)));
                    }
                } else {
                    // If both are elements, try to descend to find the exact
                    // leaf where they diverge.
                    if let (
                        HtmlNode::Element { children: ca, .. },
                        HtmlNode::Element { children: cb, .. },
                    ) = (a, b)
                    {
                        if a.kind_eq_shallow(b) {
                            if let Some(p) = diff_at(ca, cb, &child_path, shared_prefix) {
                                return Some(p);
                            }
                        }
                        // Shallow mismatch (tag name or attributes) — report here.
                        return Some(child_path);
                    }
                    // Non-element mismatch (text, comment, etc.) — report here.
                    return Some(child_path);
                }
            }
            (None, None) => {}
        }
    }
    None
}

/// Helper trait: compare only the tag name and attributes (not children).
trait HtmlNodeExt {
    fn kind_eq_shallow(&self, other: &Self) -> bool;
}

impl HtmlNodeExt for HtmlNode {
    fn kind_eq_shallow(&self, other: &Self) -> bool {
        match (self, other) {
            (
                HtmlNode::Element {
                    name: na,
                    attributes: aa,
                    ..
                },
                HtmlNode::Element {
                    name: nb,
                    attributes: ab,
                    ..
                },
            ) => na == nb && aa == ab,
            _ => false,
        }
    }
}

/// Format the diff message: show the path, the divergent nodes side-by-side,
/// and a few siblings for context.
fn format_diff(
    ours: &[HtmlNode],
    theirs: &[HtmlNode],
    path: &Path,
    shared_prefix: &VecDeque<(Path, String)>,
) -> String {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const CYAN: &str = "\x1b[36m";
    const DIM: &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";
    const BOLD: &str = "\x1b[1m";

    // Navigate to the divergent node in each tree.
    let (ours_node, theirs_node, ours_parent, theirs_parent) = navigate(ours, theirs, path);

    // Build context: up to 3 siblings before and after the divergence.
    let idx = *path.last().unwrap();
    let start = idx.saturating_sub(3);
    let end = (idx + 4).min(ours_parent.len().max(theirs_parent.len()).max(idx + 1));

    let mut lines = Vec::new();

    let path_str = path
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" > ");
    lines.push(format!(
        "{CYAN}{BOLD}First divergence at [{path_str}]{RESET}"
    ));

    if !shared_prefix.is_empty() {
        lines.push(format!("{CYAN}{BOLD}Shared prefix (last 3 nodes):{RESET}"));
        for (shared_path, summary) in shared_prefix {
            let shared_path = shared_path
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(" > ");
            lines.push(format!("  {DIM}[{shared_path}] {summary}{RESET}"));
        }
    }

    let format_side = |nodes: &[HtmlNode], label: &str, color: &str| -> String {
        let mut out = Vec::new();
        out.push(format!("  {BOLD}{label}:{RESET}"));
        for i in start..end {
            if i < nodes.len() {
                let marker = if i == idx {
                    format!(
                        "  {color}{BOLD}\u{25B8} {i}: {}{RESET}",
                        node_summary(&nodes[i])
                    )
                } else {
                    format!("  {DIM}  {i}: {}{RESET}", node_summary(&nodes[i]))
                };
                out.push(marker);
            } else if i >= idx {
                out.push(format!("  {color}{BOLD}\u{25B8} {i}: <end>{RESET}"));
            }
        }
        out.join("\n")
    };

    lines.push(format_side(ours_parent, "mdtext ", RED));
    lines.push(format_side(theirs_parent, "reference", GREEN));

    // Show full debug for the divergent nodes if they exist.
    if let Some(n) = ours_node {
        lines.push(format!("  {RED}{BOLD}mdtext  = {n:?}{RESET}"));
    } else {
        lines.push(format!("  {RED}{BOLD}mdtext  = <missing>{RESET}"));
    }
    if let Some(n) = theirs_node {
        lines.push(format!("  {GREEN}{BOLD}reference = {n:?}{RESET}"));
    } else {
        lines.push(format!("  {GREEN}{BOLD}reference = <missing>{RESET}"));
    }

    lines.join("\n")
}

/// Navigate to the divergent node and also return the sibling slice at the
/// parent level for context display.
fn navigate<'a>(
    ours: &'a [HtmlNode],
    theirs: &'a [HtmlNode],
    path: &Path,
) -> (
    Option<&'a HtmlNode>,
    Option<&'a HtmlNode>,
    &'a [HtmlNode],
    &'a [HtmlNode],
) {
    if path.is_empty() {
        return (None, None, ours, theirs);
    }
    let (last, rest) = path.split_last().unwrap();
    let mut ours_cur: &[HtmlNode] = ours;
    let mut theirs_cur: &[HtmlNode] = theirs;
    for &i in rest {
        ours_cur = match ours_cur.get(i) {
            Some(HtmlNode::Element { children, .. }) => children,
            _ => &[],
        };
        theirs_cur = match theirs_cur.get(i) {
            Some(HtmlNode::Element { children, .. }) => children,
            _ => &[],
        };
    }
    (
        ours_cur.get(*last),
        theirs_cur.get(*last),
        ours_cur,
        theirs_cur,
    )
}

#[cfg(test)]
mod tests {
    use super::{diff_trees, normalize_html};

    #[test]
    fn diff_context_uses_each_tree() {
        let ours = normalize_html("<p>ours</p>");
        let theirs = normalize_html("<strong>theirs</strong>");
        let diff = diff_trees(&ours, &theirs).unwrap();

        assert!(diff.contains("mdtext "));
        assert!(diff.contains("reference"));
        assert!(diff.contains("<p>"));
        assert!(diff.contains("<strong>"));
    }

    #[test]
    fn diff_context_shows_last_three_shared_nodes() {
        let ours =
            normalize_html("<div><p id=a>a</p><p id=b>b</p><p id=c>c</p><span>ours</span></div>");
        let theirs =
            normalize_html("<div><p id=a>a</p><p id=b>b</p><p id=c>c</p><span>theirs</span></div>");
        let diff = diff_trees(&ours, &theirs).unwrap();

        assert!(diff.contains("Shared prefix (last 3 nodes)"));
        assert!(diff.contains("id=\"a\""));
        assert!(diff.contains("id=\"b\""));
        assert!(diff.contains("id=\"c\""));
    }

    #[test]
    fn table_alignment_attribute_and_style_are_equivalent() {
        let styled = normalize_html(
            "<table><tr><th style=\"text-align: left\">a</th><td style=\"text-align: right;\">b</td></tr></table>",
        );
        let attributed = normalize_html(
            "<table><tr><th align=\"left\">a</th><td align=\"RIGHT\">b</td></tr></table>",
        );
        assert_eq!(styled, attributed);

        assert_ne!(
            normalize_html(
                "<table><tr><td style=\"color: red; text-align: right\">a</td></tr></table>"
            ),
            normalize_html("<table><tr><td align=right>a</td></tr></table>")
        );
    }

    #[test]
    fn tight_and_loose_list_paragraphs_are_equivalent() {
        let tight = normalize_html(
            "<ul>\n<li>one</li>\n<li>two\n<ul>\n<li>three</li>\n</ul>\n</li>\n</ul>",
        );
        let loose = normalize_html(
            "<ul>\n<li>\n<p>one</p>\n</li>\n<li>\n<p>two</p>\n<ul>\n<li>\n<p>three</p>\n</li>\n</ul>\n</li>\n</ul>",
        );

        assert_eq!(tight, loose);
    }

    #[test]
    fn paragraphs_nested_below_list_item_blocks_are_preserved() {
        assert_ne!(
            normalize_html("<ul><li><blockquote><p>one</p></blockquote></li></ul>"),
            normalize_html("<ul><li><blockquote>one</blockquote></li></ul>")
        );
    }
}
