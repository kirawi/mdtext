use mdtext::html::HtmlWriter;
use mdtext::{Options, Parser};
use wasm_bindgen::prelude::*;

/// A single update from [`StreamingRenderer`].
#[wasm_bindgen]
pub struct RenderUpdate {
    html_delta: String,
    consumed_bytes: usize,
    buffered_bytes: usize,
    finished: bool,
}

#[wasm_bindgen]
impl RenderUpdate {
    /// HTML emitted by this operation.
    #[wasm_bindgen(getter, js_name = htmlDelta)]
    pub fn html_delta(&self) -> String {
        self.html_delta.clone()
    }

    /// Number of bytes discarded from the parser's pending input buffer. These should NOT be retained on subsequent pushes.
    #[wasm_bindgen(getter, js_name = consumedBytes)]
    pub fn consumed_bytes(&self) -> usize {
        self.consumed_bytes
    }

    /// Number of bytes retained because the parser needs more input.
    #[wasm_bindgen(getter, js_name = bufferedBytes)]
    pub fn buffered_bytes(&self) -> usize {
        self.buffered_bytes
    }

    /// Whether the renderer has been finalized.
    #[wasm_bindgen(getter)]
    pub fn finished(&self) -> bool {
        self.finished
    }
}

/// Incrementally parses Markdown and emits an HTML stream.
#[wasm_bindgen]
pub struct StreamingRenderer {
    parser: Parser,
    writer: HtmlWriter,
    pending: String,
    finished: bool,
}

#[wasm_bindgen]
impl StreamingRenderer {
    #[wasm_bindgen(constructor)]
    pub fn new(options_bits: u32) -> Self {
        let options = Options::from_bits(options_bits);
        Self {
            parser: Parser::with_options(options),
            writer: HtmlWriter::with_options(options),
            pending: String::new(),
            finished: false,
        }
    }

    /// Feed another chunk into the renderer.
    pub fn push(&mut self, chunk: &str) -> Result<RenderUpdate, JsError> {
        if self.finished {
            return Err(JsError::new("cannot push after finish"));
        }

        self.pending.push_str(chunk);
        let (events, consumed_bytes) = self.parser.feed_chunk(&self.pending);
        for event in &events {
            self.writer.push_event(event);
        }
        drop(events);

        if consumed_bytes > 0 {
            self.pending.drain(..consumed_bytes);
        }

        Ok(RenderUpdate {
            html_delta: self.writer.take_output(),
            consumed_bytes,
            buffered_bytes: self.pending.len(),
            finished: false,
        })
    }

    /// Flush buffered input and close all open Markdown blocks.
    pub fn finish(&mut self) -> Result<RenderUpdate, JsError> {
        if self.finished {
            return Err(JsError::new("renderer is already finished"));
        }

        let consumed_bytes = self.pending.len();
        let events = self.parser.finish(&self.pending);
        for event in &events {
            self.writer.push_event(event);
        }
        drop(events);
        self.pending.clear();
        self.finished = true;

        Ok(RenderUpdate {
            html_delta: self.writer.take_output(),
            consumed_bytes,
            buffered_bytes: 0,
            finished: true,
        })
    }
}

/// Render a complete Markdown string in one call.
#[wasm_bindgen]
pub fn render(markdown: &str, options_bits: u32) -> String {
    let options = Options::from_bits(options_bits);
    let mut writer = HtmlWriter::with_options(options);
    for event in Parser::parse_str(markdown, options) {
        writer.push_event(&event);
    }
    writer.into_string()
}

/// Return the bitset for all GitHub-flavored Markdown extensions.
#[wasm_bindgen(js_name = gfmOptions)]
pub fn gfm_options() -> u32 {
    (Options::GFM | Options::SKIP_ROOT_DEFERRED).bits()
}

/// Return the bitset for all supported math syntaxes.
#[wasm_bindgen(js_name = mathOptions)]
pub fn math_options() -> u32 {
    Options::MATH.bits()
}
