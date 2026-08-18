// WARNING: the entirety of this file was LLM-generated. BEWARE!

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use mdtext::{Event, Options, Parser, Tag};

mod web;

const USAGE: &str = "usage: mdtext [--gfm] [--math] [FILE ...]\n       mdtext --web [--paper]";

fn main() {
    let mut options = Options::empty();
    let mut paths = Vec::new();
    let mut web_mode = false;
    let mut paper_style = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--gfm" => options |= Options::GFM,
            "--math" => options |= Options::MATH,
            "--web" => web_mode = true,
            "--paper" => paper_style = true,
            "-h" | "--help" => {
                eprintln!("{USAGE}");
                return;
            }
            flag if flag.starts_with('-') => {
                eprintln!("unknown option: {flag}");
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
            path => paths.push(path.to_string()),
        }
    }

    if paper_style && !web_mode {
        eprintln!("--paper requires --web");
        eprintln!("{USAGE}");
        std::process::exit(2);
    }

    if web_mode {
        if !paths.is_empty() {
            eprintln!("--web does not accept file paths");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
        // The interactive preview is intended to match a modern chat
        // renderer, so its useful extensions work without extra flags.
        options |= Options::GFM | Options::MATH | Options::MATH_LATEX;
        if let Err(error) = web::run(options, paper_style) {
            eprintln!("web preview failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    if paths.is_empty() {
        // Demo mode: stdin → event stream.
        let mut input = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut input) {
            eprintln!("error reading stdin: {e}");
            std::process::exit(1);
        }
        for event in Parser::parse_str(&input, options) {
            print_event(&event);
        }
        return;
    }

    for path in &paths {
        if let Err(e) = convert_file(Path::new(path), options) {
            eprintln!("error converting {}: {}", path, e);
            std::process::exit(1);
        }
    }
}

fn convert_file(input: &Path, options: Options) -> io::Result<()> {
    let mut file = File::open(input)?;
    let mut markdown = String::new();
    file.read_to_string(&mut markdown)?;

    let mut writer = mdtext::html::HtmlWriter::with_options(options);
    writer.push_text_raw(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n</head>\n<body>\n",
    );
    for event in Parser::parse_str(&markdown, options) {
        writer.push_event(&event);
    }
    writer.push_text_raw("</body>\n</html>\n");

    let output = input.with_extension("html");
    let mut out = File::create(&output)?;
    out.write_all(writer.into_string().as_bytes())?;
    eprintln!("wrote {}", output.display());
    Ok(())
}

fn print_event(event: &Event) {
    match event {
        Event::Start(tag) => {
            println!("Start({})", format_tag(tag));
        }
        Event::End => {
            println!("End");
        }
        Event::Text(s) => {
            println!("Text({:?})", s);
        }
        Event::Code(s) => {
            println!("Code({:?})", s);
        }
        Event::SoftBreak => {
            println!("SoftBreak");
        }
        Event::HardBreak => {
            println!("HardBreak");
        }
        Event::ThematicBreak => {
            println!("ThematicBreak");
        }
        Event::Html(s) => {
            println!("Html({:?})", s);
        }
        Event::TaskListMarker(checked) => println!("TaskListMarker({checked})"),
        Event::InlineMath(source) => println!("InlineMath({source:?})"),
        Event::DisplayMath(source) => println!("DisplayMath({source:?})"),
    }
}

fn format_tag(tag: &Tag) -> String {
    match tag {
        Tag::Paragraph => "Paragraph".to_string(),
        Tag::Heading(level) => format!("Heading({})", level),
        Tag::CodeBlock(kind) => format!("CodeBlock({:?})", kind),
        Tag::CodeSpan => "CodeSpan".to_string(),
        Tag::DisplayMath => "DisplayMath".to_string(),
        Tag::HtmlBlock => "HtmlBlock".to_string(),
        Tag::Quote => "BlockQuote".to_string(),
        Tag::List(kind) => format!("List({:?})", kind),
        Tag::Item => "Item".to_string(),
        Tag::Emphasis => "Emphasis".to_string(),
        Tag::Strong => "Strong".to_string(),
        Tag::Strikethrough => "Strikethrough".to_string(),
        Tag::Table(alignments) => format!("Table({alignments:?})"),
        Tag::TableHead => "TableHead".to_string(),
        Tag::TableBody => "TableBody".to_string(),
        Tag::TableRow => "TableRow".to_string(),
        Tag::TableCell => "TableCell".to_string(),
        Tag::Link { url, title } => {
            format!("Link {{ url: {:?}, title: {:?} }}", url, title)
        }
        Tag::Image { url, title } => {
            format!("Image {{ url: {:?}, title: {:?} }}", url, title)
        }
    }
}
