// WARNING: This entire file is AI-generated. BEWARE!

//! Local browser-based live Markdown preview.

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Duration;

    use mdtext::{Event, Options, Parser, Tag};

    use mdtext::html::HtmlWriter;

    const MAX_HEADER_BYTES: usize = 64 * 1024;
    const MAX_MARKDOWN_BYTES: u64 = 8 * 1024 * 1024;
    const MATH_COMPLETE_MARKER: &str = "<!--mdtext-math-complete-->";
    const PREVIEW_DOCUMENT_HEAD: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline' https://cdn.jsdelivr.net; img-src data: https: http:; font-src https://cdn.jsdelivr.net">
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/katex@0.16.22/dist/katex.min.css" integrity="sha384-5TcZemv2l/9On385z///+d7MSYlvIEw9FuZTIdZ14vJLqWphw7e7ZPuOiCHJcFCP" crossorigin="anonymous">
<style>
:root { color-scheme: light dark; --ink:#242927; --muted:#66706b; --line:#dce1dd; --soft:#f3f5f3; --accent:#13795b; }
* { box-sizing:border-box; }
html { overflow-y:scroll; }
body { max-width:900px; margin:0 auto; padding:30px 34px 80px; color:var(--ink); font:16px/1.68 ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; overflow-wrap:anywhere; }
h1,h2,h3,h4,h5,h6 { line-height:1.22; margin:1.45em 0 .65em; letter-spacing:-.025em; }
h1:first-child,h2:first-child { margin-top:0; }
h1 { font-size:2em; padding-bottom:.28em; border-bottom:1px solid var(--line); }
h2 { font-size:1.5em; padding-bottom:.25em; border-bottom:1px solid var(--line); }
a { color:var(--accent); }
blockquote { margin:1.2em 0; padding:.1em 1em; color:var(--muted); border-left:4px solid var(--line); }
code { padding:.14em .36em; border-radius:5px; background:var(--soft); font:85% ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; }
pre { overflow:auto; padding:16px; border:1px solid var(--line); border-radius:9px; background:var(--soft); }
pre code { padding:0; background:none; font-size:13px; }
table { width:100%; border-spacing:0; border-collapse:collapse; margin:1.25em 0; }
th,td { padding:8px 12px; border:1px solid var(--line); }
th { background:var(--soft); }
hr { height:1px; border:0; background:var(--line); margin:2em 0; }
input[type=checkbox] { margin-right:.35em; accent-color:var(--accent); }
li > input[type=checkbox] + p { display:inline; }
.math { font-family:ui-serif,Georgia,serif; }
.math-display { display:block; margin:1.2em 0; padding:12px 16px; text-align:center; background:var(--soft); border-radius:8px; }
.empty { color:var(--muted); }
#following { position:fixed; right:24px; bottom:20px; padding:7px 14px; border:1px solid var(--line); border-radius:999px; background:#1d211f; color:#e6ebe7; font:12px/1.3 ui-sans-serif,system-ui,sans-serif; box-shadow:0 6px 18px rgba(0,0,0,.22); cursor:pointer; }
@media (prefers-color-scheme:dark) { :root { --ink:#e5ebe7; --muted:#9aa69f; --line:#38413c; --soft:#252b28; --accent:#57c6a3; } body { background:#1e2220; } }
@media (max-width:600px) { body { padding:22px 20px 60px; } }

/* Optional formal-paper presentation. The parser output is unchanged; the
   --paper flag only adds this class to the rendered document body. */
body.paper {
  max-width:820px;
  padding:42px 48px 80px;
  overflow-wrap:break-word;
  hyphens:auto;
  font:17px/1.58 "KaTeX_Main","STIX Two Text","Latin Modern Roman","Computer Modern Serif",Cambria,"Times New Roman",serif;
}
body.paper p { margin:.72em 0; text-align:justify; }
body.paper h1,
body.paper h2,
body.paper h3,
body.paper h4,
body.paper h5,
body.paper h6 {
  color:var(--ink);
  font-family:"KaTeX_Main","STIX Two Text","Latin Modern Roman","Computer Modern Serif",Cambria,"Times New Roman",serif;
  font-weight:600;
  line-height:1.2;
  letter-spacing:0;
  break-after:avoid;
}
body.paper h1 { margin:.1em 0 1.1em; padding:0; border:0; text-align:center; font-size:2.05em; font-weight:500; }
body.paper h2 { margin:1.7em 0 .55em; padding:0; border:0; font-size:1.42em; }
body.paper h3 { margin:1.45em 0 .45em; font-size:1.16em; }
body.paper h4,
body.paper h5,
body.paper h6 { margin:1.25em 0 .35em; font-size:1em; }
body.paper a { color:var(--accent); text-decoration-thickness:.06em; text-underline-offset:.12em; }
body.paper blockquote { margin:1.1em 2.3em; padding:0; border:0; color:var(--muted); font-style:italic; }
body.paper ul,
body.paper ol { margin:.7em 0 .9em; padding-left:2.15em; }
body.paper li { margin:.18em 0; }
body.paper code { padding:.08em .25em; border-radius:2px; background:var(--soft); font:82%/1.4 "KaTeX_Typewriter","Latin Modern Mono","Courier New",monospace; }
body.paper pre { margin:1.15em 0; padding:12px 15px; border:1px solid var(--line); border-radius:3px; background:var(--soft); line-height:1.42; }
body.paper pre code { padding:0; background:none; font-size:13px; }
body.paper table { width:100%; margin:1.3em auto; border-top:1.5px solid var(--ink); border-bottom:1.5px solid var(--ink); font-size:.91em; }
body.paper th,
body.paper td { padding:5px 9px; border:0; text-align:left; vertical-align:top; }
body.paper th { border-bottom:1px solid var(--muted); background:transparent; font-weight:600; }
body.paper tbody tr + tr td { border-top:1px solid var(--line); }
body.paper hr { height:0; margin:1.7em auto; border:0; border-top:1px solid var(--muted); background:none; }
body.paper .math { font-family:"STIX Two Math","Cambria Math","Times New Roman",serif; }
body.paper .math-display { margin:1.35em 0; padding:0; background:transparent; border-radius:0; }
body.paper #following { font-family:ui-sans-serif,system-ui,sans-serif; }
@media (max-width:600px) {
  body.paper { padding:30px 22px 60px; font-size:16px; }
  body.paper blockquote { margin-left:1.2em; margin-right:1.2em; }
}
@media print {
  body.paper { max-width:none; padding:0; font-size:11pt; }
  body.paper #following { display:none; }
}
</style></head>"#;
    const PREVIEW_BODY_OPEN: &str = "<body id=\"mdtext-preview-body\">";
    const PAPER_PREVIEW_BODY_OPEN: &str = "<body id=\"mdtext-preview-body\" class=\"paper\">";
    const PREVIEW_DOCUMENT_TAIL: &str = "</body></html>";

    pub fn run(options: Options, paper_style: bool) -> io::Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let url = format!("http://{address}/");

        eprintln!("mdtext live preview: {url}");
        eprintln!("press Ctrl-C to stop");
        if let Err(error) = open_browser(&url) {
            eprintln!("could not open a browser automatically: {error}");
            eprintln!("open {url} manually");
        }

        loop {
            let (stream, _) = listener.accept()?;
            thread::spawn(move || {
                if let Err(error) = handle_connection(stream, options, paper_style)
                    && !is_disconnect(&error)
                {
                    eprintln!("web preview request failed: {error}");
                }
            });
        }
    }

    fn open_browser(url: &str) -> io::Result<()> {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "start", "", url]);
            command
        };

        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = Command::new("open");
            command.arg(url);
            command
        };

        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut command = Command::new("xdg-open");
            command.arg(url);
            command
        };

        #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "automatic browser opening is unsupported on this platform",
        ));

        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
    }

    fn handle_connection(
        mut stream: TcpStream,
        options: Options,
        paper_style: bool,
    ) -> io::Result<()> {
        let timeout = Some(Duration::from_secs(30));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;

        // Keep request buffering independent from the response writer. The
        // parser consumes the request body directly through this BufReader,
        // so a render never needs to buffer the complete Markdown document.
        let mut reader = BufReader::new(stream.try_clone()?);
        let request = match read_request_head(&mut reader) {
            Ok(request) => request,
            Err(error) => {
                write_text_response(&mut stream, "400 Bad Request", &error.to_string())?;
                return Ok(());
            }
        };
        let path = request.path.split('?').next().unwrap_or(&request.path);

        match (request.method.as_str(), path) {
            ("GET", "/") => write_fixed_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                WEB_PAGE.as_bytes(),
            ),
            ("GET", "/favicon.ico") => {
                write_fixed_response(&mut stream, "204 No Content", "image/x-icon", &[])
            }
            ("POST", "/render") => {
                let Some(content_length) = request.content_length else {
                    return write_text_response(
                        &mut stream,
                        "411 Length Required",
                        "Content-Length is required",
                    );
                };
                if content_length > MAX_MARKDOWN_BYTES {
                    return write_text_response(
                        &mut stream,
                        "413 Content Too Large",
                        "Markdown input exceeds the 8 MiB limit",
                    );
                }
                write_render_response(&mut stream, reader.take(content_length), options)
            }
            ("POST", "/render-frame") => {
                let Some(content_length) = request.content_length else {
                    return write_text_response(
                        &mut stream,
                        "411 Length Required",
                        "Content-Length is required",
                    );
                };
                if content_length > MAX_MARKDOWN_BYTES {
                    return write_text_response(
                        &mut stream,
                        "413 Content Too Large",
                        "Markdown input exceeds the 8 MiB limit",
                    );
                }
                write_frame_render_response(
                    &mut stream,
                    reader.take(content_length),
                    options,
                    paper_style,
                )
            }
            _ => write_text_response(&mut stream, "404 Not Found", "not found"),
        }
    }

    #[derive(Debug)]
    struct RequestHead {
        method: String,
        path: String,
        content_length: Option<u64>,
    }

    fn read_request_head<R: BufRead>(reader: &mut R) -> io::Result<RequestHead> {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "empty HTTP request",
            ));
        }
        let mut parts = request_line.split_ascii_whitespace();
        let method = parts.next().unwrap_or_default();
        let path = parts.next().unwrap_or_default();
        let version = parts.next().unwrap_or_default();
        if method.is_empty()
            || path.is_empty()
            || !version.starts_with("HTTP/")
            || parts.next().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed HTTP request line",
            ));
        }

        let mut header_bytes = request_line.len();
        let mut content_length = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete HTTP headers",
                ));
            }
            header_bytes += line.len();
            if header_bytes > MAX_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP headers exceed 64 KiB",
                ));
            }
            if line == "\r\n" || line == "\n" {
                break;
            }

            let Some((name, value)) = line.split_once(':') else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed HTTP header",
                ));
            };
            if name.eq_ignore_ascii_case("content-length") {
                let parsed = value.trim().parse::<u64>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
                })?;
                if content_length.is_some_and(|length| length != parsed) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "conflicting Content-Length headers",
                    ));
                }
                content_length = Some(parsed);
            }
        }

        Ok(RequestHead {
            method: method.to_string(),
            path: path.to_string(),
            content_length,
        })
    }

    fn write_render_response<W: Write, R: BufRead>(
        output: &mut W,
        markdown: R,
        options: Options,
    ) -> io::Result<()> {
        write_render_stream(output, markdown, options, false, false)
    }

    fn write_frame_render_response<W: Write, R: Read>(
        output: &mut W,
        mut form_body: R,
        options: Options,
        paper_style: bool,
    ) -> io::Result<()> {
        let mut body = String::new();
        form_body.read_to_string(&mut body)?;
        let markdown = body.strip_prefix("markdown=").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing Markdown form field")
        })?;
        let markdown = markdown.strip_suffix("\r\n").unwrap_or(markdown);
        write_render_stream(
            output,
            Cursor::new(markdown.as_bytes()),
            options,
            true,
            paper_style,
        )
    }

    fn write_render_stream<W: Write, R: BufRead>(
        output: &mut W,
        mut markdown: R,
        options: Options,
        include_document: bool,
        paper_style: bool,
    ) -> io::Result<()> {
        output.write_all(
            b"HTTP/1.1 200 OK\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Transfer-Encoding: chunked\r\n\
Cache-Control: no-store\r\n\
X-Content-Type-Options: nosniff\r\n\
Connection: close\r\n\r\n",
        )?;
        output.flush()?;

        if include_document {
            write_chunk(output, PREVIEW_DOCUMENT_HEAD.as_bytes())?;
            let body_open = if paper_style {
                PAPER_PREVIEW_BODY_OPEN
            } else {
                PREVIEW_BODY_OPEN
            };
            write_chunk(output, body_open.as_bytes())?;
            output.flush()?;
        }

        // Throttle the *input* so the chunked HTTP response visibly streams
        // to the browser instead of arriving in a single burst.  Each line of
        // the request body is fed to the parser after a short delay, exercising
        // the real streaming feed path rather than buffering the document.
        const STREAM_DELAY: Duration = Duration::from_millis(0);

        let mut renderer = HtmlWriter::with_options(options);
        let mut parser = Parser::with_options(options);
        let mut math_stack = Vec::new();
        // Accumulator for the unconsumed input tail.  `dropped` tracks how many
        // bytes at the front the parser has fully consumed and the caller may
        // reclaim; `buffer[dropped..]` is the live window re-fed on each line.
        let mut buffer = String::new();
        let mut dropped = 0usize;
        let mut line = String::new();

        loop {
            line.clear();
            if markdown.read_line(&mut line)? == 0 {
                break;
            }
            thread::sleep(STREAM_DELAY);
            buffer.push_str(&line);

            let consumed = {
                let mut it = parser.feed(&buffer[dropped..]);
                for event in &mut it {
                    let completes_math = event_completes_math(&event, &mut math_stack);
                    renderer.push_event(&event);
                    let mut fragment = renderer.take_output();
                    if !fragment.is_empty() {
                        if completes_math {
                            fragment.push_str(MATH_COMPLETE_MARKER);
                        }
                        write_chunk(output, fragment.as_bytes())?;
                        output.flush()?;
                    }
                }
                it.consumed()
            };
            dropped += consumed;
        }

        // Flush end-of-input: parse any trailing partial line, then close all
        // open blocks.
        for event in parser.finish_iter(&buffer[dropped..]) {
            let completes_math = event_completes_math(&event, &mut math_stack);
            renderer.push_event(&event);
            let mut fragment = renderer.take_output();
            if !fragment.is_empty() {
                if completes_math {
                    fragment.push_str(MATH_COMPLETE_MARKER);
                }
                write_chunk(output, fragment.as_bytes())?;
                output.flush()?;
            }
        }

        if include_document {
            write_chunk(output, PREVIEW_DOCUMENT_TAIL.as_bytes())?;
            output.flush()?;
        }
        output.write_all(b"0\r\n\r\n")?;
        output.flush()
    }

    fn event_completes_math(event: &Event, stack: &mut Vec<bool>) -> bool {
        match event {
            Event::InlineMath(_) => true,
            Event::Start(tag) => {
                stack.push(matches!(tag, Tag::DisplayMath));
                false
            }
            Event::End => stack.pop().unwrap_or(false),
            _ => false,
        }
    }

    fn write_chunk<W: Write>(output: &mut W, bytes: &[u8]) -> io::Result<()> {
        write!(output, "{:x}\r\n", bytes.len())?;
        output.write_all(bytes)?;
        output.write_all(b"\r\n")
    }

    fn write_text_response<W: Write>(
        output: &mut W,
        status: &str,
        message: &str,
    ) -> io::Result<()> {
        write_fixed_response(
            output,
            status,
            "text/plain; charset=utf-8",
            message.as_bytes(),
        )
    }

    fn write_fixed_response<W: Write>(
        output: &mut W,
        status: &str,
        content_type: &str,
        body: &[u8],
    ) -> io::Result<()> {
        write!(
            output,
            "HTTP/1.1 {status}\r\n\
Content-Type: {content_type}\r\n\
Content-Length: {}\r\n\
Cache-Control: no-store\r\n\
X-Content-Type-Options: nosniff\r\n\
Content-Security-Policy: default-src 'self'; style-src 'unsafe-inline' https://cdn.jsdelivr.net; script-src 'unsafe-inline' https://cdn.jsdelivr.net; connect-src 'self'; frame-src 'self'; font-src https://cdn.jsdelivr.net\r\n\
Referrer-Policy: no-referrer\r\n\
Connection: close\r\n\r\n",
            body.len()
        )?;
        output.write_all(body)?;
        output.flush()
    }

    fn is_disconnect(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::UnexpectedEof
        )
    }

    const WEB_PAGE: &str = r###"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>mdtext live preview</title>
<style>
:root {
  color-scheme: light dark;
  --bg: #f6f5f1;
  --panel: #ffffff;
  --ink: #1d211f;
  --muted: #66706b;
  --line: #d9ddd9;
  --accent: #13795b;
  --accent-soft: #dff4eb;
  --editor: #202523;
  --editor-ink: #e9efec;
}
* { box-sizing: border-box; }
html, body { height: 100%; }
body {
  margin: 0;
  overflow: hidden;
  background: var(--bg);
  color: var(--ink);
  font: 14px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
.app { height: 100%; display: grid; grid-template-rows: 58px minmax(0, 1fr) 30px; }
header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 20px;
  border-bottom: 1px solid var(--line);
  background: color-mix(in srgb, var(--panel) 92%, transparent);
}
.brand { display: flex; align-items: center; gap: 11px; font-weight: 720; letter-spacing: -.02em; }
.mark {
  display: grid;
  place-items: center;
  width: 30px;
  height: 30px;
  border-radius: 9px;
  color: white;
  background: var(--accent);
  font: 800 13px/1 ui-monospace, SFMono-Regular, Consolas, monospace;
  box-shadow: 0 5px 15px color-mix(in srgb, var(--accent) 24%, transparent);
}
.mode { color: var(--muted); font-size: 12px; }
main { min-height: 0; display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }
.pane { min-width: 0; min-height: 0; display: grid; grid-template-rows: 38px minmax(0, 1fr); }
.pane + .pane { border-left: 1px solid var(--line); }
.pane-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 14px;
  background: var(--panel);
  border-bottom: 1px solid var(--line);
  color: var(--muted);
  font-size: 11px;
  font-weight: 750;
  letter-spacing: .09em;
  text-transform: uppercase;
}
.hint { font-weight: 500; letter-spacing: 0; text-transform: none; opacity: .75; }
textarea {
  width: 100%;
  height: 100%;
  resize: none;
  border: 0;
  outline: 0;
  padding: 24px;
  background: var(--editor);
  color: var(--editor-ink);
  caret-color: #73d4b6;
  tab-size: 2;
  font: 14px/1.62 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}
iframe { width: 100%; height: 100%; border: 0; background: var(--panel); }
footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 14px;
  border-top: 1px solid var(--line);
  color: var(--muted);
  background: var(--panel);
  font-size: 11px;
}
.dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); }
.busy .dot { animation: pulse .8s ease-in-out infinite alternate; }
@keyframes pulse { to { opacity: .25; transform: scale(.75); } }
@media (max-width: 760px) {
  body { overflow: auto; }
  .app { height: auto; min-height: 100%; grid-template-rows: 58px auto 30px; }
  main { grid-template-columns: 1fr; grid-template-rows: 48vh 52vh; }
  .pane + .pane { border-left: 0; border-top: 1px solid var(--line); }
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #171a19;
    --panel: #1e2220;
    --ink: #e6ebe8;
    --muted: #9ba6a0;
    --line: #363d39;
    --accent: #42b892;
    --accent-soft: #173d31;
    --editor: #111412;
    --editor-ink: #e2eae6;
  }
}
</style>
</head>
<body>
<div class="app">
  <header>
    <div class="brand"><span class="mark">md</span><span>mdtext live preview</span></div>
    <div class="mode">GFM + math · streamed locally</div>
  </header>
  <main>
    <section class="pane">
      <div class="pane-title"><span>Markdown</span><span class="hint">Ctrl/⌘ + S to render</span></div>
      <textarea id="editor" aria-label="Markdown editor" autofocus spellcheck="false"># Live Markdown

Paste or type here. Rendering updates as you type.

| Feature | Status |
| :--- | ---: |
| Tables | **Ready** |
| Streaming | ~~Queued~~ Live |

- [x] GFM extensions
- [x] Sandboxed preview

Inline math: $x^2 + y^2$</textarea>
    </section>
    <section class="pane">
      <div class="pane-title"><span>Preview</span><span class="hint" id="size"></span></div>
      <iframe id="preview" name="mdtext-preview" title="Rendered Markdown preview" sandbox="allow-same-origin"></iframe>
    </section>
  </main>
  <footer id="status"><span class="dot"></span><span id="statusText">Starting renderer…</span></footer>
</div>
<script src="https://cdn.jsdelivr.net/npm/katex@0.16.22/dist/katex.min.js" integrity="sha384-cMkvdD8LoxVzGF/RPUKAcvmm49FQ0oxwDF3BGKtDXcEc+T1b2N+teh/OJfpU0jr6" crossorigin="anonymous"></script>
<script>
const editor = document.querySelector('#editor');
const preview = document.querySelector('#preview');
const status = document.querySelector('#status');
const statusText = document.querySelector('#statusText');
const size = document.querySelector('#size');
let debounce;
let generation = 0;
let paintQueued = false;
let streamComplete = true;
let observedFrameDocument = null;
let frameObserver = null;

const renderForm = document.createElement('form');
renderForm.hidden = true;
renderForm.method = 'POST';
renderForm.enctype = 'text/plain';
renderForm.target = preview.name;
const renderField = document.createElement('textarea');
renderField.name = 'markdown';
renderForm.appendChild(renderField);
document.body.appendChild(renderForm);

// `follow` sticks to new streamed content until the user wheels upward. It
// resumes when they return to the bottom or click the pause indicator.
let follow = true;
const RESUME_THRESHOLD = 96; // tolerate one newly-laid-out block while resuming

function ensureFollowIndicator(doc) {
  if (!doc.body || doc.querySelector('#following')) return;
  const indicator = doc.createElement('button');
  indicator.id = 'following';
  indicator.type = 'button';
  indicator.textContent = '↓ Following paused — scroll to bottom to resume';
  indicator.addEventListener('click', () => {
    follow = true;
    doc.documentElement.scrollTop = doc.documentElement.scrollHeight;
    indicator.remove();
  });
  doc.body.appendChild(indicator);
}

function onFrameScroll(event) {
  const doc = event.currentTarget;
  if (doc !== preview.contentDocument) return;
  const el = doc.documentElement;
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < RESUME_THRESHOLD;
  if (!follow && atBottom) {
    follow = true;
    el.scrollTop = el.scrollHeight;
    const indicator = doc.querySelector('#following');
    if (indicator) indicator.remove();
  }
}

function onFrameWheel(event) {
  if (event.deltaY >= 0 || !follow) return;
  const doc = event.currentTarget;
  if (doc !== preview.contentDocument) return;
  follow = false;
  ensureFollowIndicator(doc);
}

function observeFrame(doc) {
  doc.addEventListener('scroll', onFrameScroll, { passive: true });
  doc.addEventListener('wheel', onFrameWheel, { passive: true });
}

function observeStreamingFrame(doc) {
  if (!doc || doc === observedFrameDocument || doc.body?.id !== 'mdtext-preview-body') {
    return false;
  }
  if (frameObserver) frameObserver.disconnect();
  observedFrameDocument = doc;
  frameObserver = new MutationObserver(schedulePaint);
  frameObserver.observe(doc, { childList: true, subtree: true, characterData: true });
  observeFrame(doc);
  schedulePaint();
  return true;
}

function monitorFrame(current) {
  if (current !== generation || streamComplete) return;
  const doc = preview.contentDocument;
  observeStreamingFrame(doc);
  if (doc === observedFrameDocument) schedulePaint();
  setTimeout(() => monitorFrame(current), 50);
}

preview.addEventListener('load', () => {
  const location = preview.contentWindow.location;
  const loadedGeneration = Number(new URL(location.href).searchParams.get('generation'));
  if (loadedGeneration !== generation) return;

  const doc = preview.contentDocument;
  if (!observeStreamingFrame(doc) && doc !== observedFrameDocument) {
    statusText.textContent = 'Render failed: preview document did not load';
    status.classList.remove('busy');
    return;
  }
  streamComplete = true;
  schedulePaint();
  statusText.textContent = 'Rendered';
  status.classList.remove('busy');
});

function schedulePaint() {
  if (paintQueued) return;
  paintQueued = true;
  requestAnimationFrame(() => {
    paintQueued = false;
    const doc = preview.contentDocument;
    if (!doc || !doc.body) return;
    renderMath(doc.body);
    if (follow) {
      doc.documentElement.scrollTop = doc.documentElement.scrollHeight;
    } else {
      ensureFollowIndicator(doc);
    }
  });
}

function renderMath(root) {
  if (typeof katex === 'undefined') return;
  root.querySelectorAll('.math:not([data-mdtext-rendered])').forEach(el => {
    let marker = el.nextSibling;
    while (marker?.nodeType === 3 && !marker.data.trim()) {
      marker = marker.nextSibling;
    }
    const hasCompletionMarker = marker?.nodeType === 8
      && marker.data === 'mdtext-math-complete';
    if (!streamComplete && !hasCompletionMarker) return;
    if (hasCompletionMarker) marker.remove();
    el.dataset.mdtextRendered = 'true';
    const tex = el.textContent;
    if (!tex) return;
    const displayMode = el.classList.contains('math-display');
    try {
      el.innerHTML = katex.renderToString(tex, { displayMode, throwOnError: false });
    } catch (error) {
      el.classList.add('math-error');
      el.setAttribute('title', error.message);
    }
  });
}

function render() {
  clearTimeout(debounce);
  const current = ++generation;
  streamComplete = false;
  status.classList.add('busy');
  statusText.textContent = 'Streaming render…';
  size.textContent = `${editor.value.length.toLocaleString()} characters`;
  renderField.value = editor.value;
  renderForm.action = `/render-frame?generation=${current}`;
  renderForm.requestSubmit();
  requestAnimationFrame(() => monitorFrame(current));
}

editor.addEventListener('input', () => {
  clearTimeout(debounce);
  debounce = setTimeout(render, 70);
});
window.addEventListener('keydown', event => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
    event.preventDefault();
    render();
  }
});
render();
</script>
</body>
</html>
"###;

    #[cfg(test)]
    mod tests {
        use std::io::Cursor;

        use super::{
            MATH_COMPLETE_MARKER, WEB_PAGE, write_frame_render_response, write_render_response,
        };
        use mdtext::Options;

        #[test]
        fn page_contains_live_streaming_editor() {
            assert!(WEB_PAGE.contains("<textarea id=\"editor\""));
            assert!(WEB_PAGE.contains("sandbox=\"allow-same-origin\""));
            assert!(WEB_PAGE.contains("renderForm.enctype = 'text/plain'"));
            assert!(WEB_PAGE.contains("renderForm.requestSubmit()"));
            assert!(WEB_PAGE.contains("/render-frame?generation="));
            assert!(WEB_PAGE.contains("event.deltaY >= 0 || !follow"));
            assert!(!WEB_PAGE.contains("doc.write("));
            assert!(!WEB_PAGE.contains("doc.body.innerHTML = pendingHtml"));
        }

        #[test]
        fn render_response_streams_gfm_as_chunked_html() {
            let markdown = Cursor::new(b"a | b\n--- | ---\n~~x~~ | $y$\n");
            let mut response = Vec::new();
            write_render_response(&mut response, markdown, Options::GFM | Options::MATH).unwrap();

            let html = decode_chunked_body(&response);
            assert!(html.contains("<table>"));
            assert!(html.contains("<del>x</del>"));
            assert!(html.contains("<span class=\"math math-inline\">y</span>"));
            assert!(html.contains(MATH_COMPLETE_MARKER));
        }

        #[test]
        fn frame_response_is_a_complete_streamed_document() {
            let form = Cursor::new(b"markdown=# heading\r\n" as &[u8]);
            let mut response = Vec::new();
            write_frame_render_response(&mut response, form, Options::GFM | Options::MATH, false)
                .unwrap();

            let html = decode_chunked_body(&response);
            assert!(html.starts_with("<!doctype html>"));
            assert!(html.contains("<body id=\"mdtext-preview-body\">"));
            assert!(html.contains("<h1>heading</h1>"));
            assert!(html.ends_with("</body></html>"));
        }

        #[test]
        fn paper_frame_activates_typography_without_forcing_light_mode() {
            let form = Cursor::new(b"markdown=# Formal title\r\n" as &[u8]);
            let mut response = Vec::new();
            write_frame_render_response(&mut response, form, Options::GFM | Options::MATH, true)
                .unwrap();

            let html = decode_chunked_body(&response);
            assert!(html.contains("<body id=\"mdtext-preview-body\" class=\"paper\">"));
            assert!(html.contains("body.paper p { margin:.72em 0; text-align:justify; }"));
            assert!(!html.contains("html:has(body.paper)"));
            assert!(!html.contains("body.paper {\n  color-scheme:light;"));
            assert!(html.contains("<h1>Formal title</h1>"));
        }

        fn decode_chunked_body(response: &[u8]) -> String {
            let body_start = response
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let mut position = body_start;
            let mut body = Vec::new();
            loop {
                let line_end = response[position..]
                    .windows(2)
                    .position(|window| window == b"\r\n")
                    .unwrap()
                    + position;
                let size = usize::from_str_radix(
                    std::str::from_utf8(&response[position..line_end]).unwrap(),
                    16,
                )
                .unwrap();
                position = line_end + 2;
                if size == 0 {
                    break;
                }
                body.extend_from_slice(&response[position..position + size]);
                position += size + 2;
            }
            String::from_utf8(body).unwrap()
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::run;

#[cfg(target_arch = "wasm32")]
pub fn run(_options: mdtext::Options, _paper_style: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "--web requires a native target with TCP sockets",
    ))
}
