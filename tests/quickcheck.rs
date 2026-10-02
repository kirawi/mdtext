use mdtext::{Options, Parser};
use mdtext::html::HtmlWriter;
use quickcheck::QuickCheck;

const SUPPORTED_OPTIONS: u64 = Options::GFM.bits() | Options::MATH.bits() | Options::IMMEDIATE_MODE.bits();

#[test]
fn streaming_and_complete_parsing_produce_identical_html() {
    fn property(source: String, flags: u16, chunk_size: u8) -> bool {
        for options in [
            Options::empty(),
            Options::GFM,
            Options::from_bits(SUPPORTED_OPTIONS),
            Options::from_bits(u64::from(flags) & SUPPORTED_OPTIONS),
        ] {
            let mut complete = HtmlWriter::with_options(options);
            for event in Parser::parse_str(&source, options) {
                complete.push_event(&event);
            }

            let mut parser = Parser::with_options(options);
            let mut streaming = HtmlWriter::with_options(options);
            let mut pending = String::new();
            let mut start = 0;
            let chunk_chars = usize::from(chunk_size % 16) + 1;
            for end in source.char_indices().skip(chunk_chars).step_by(chunk_chars)
                .map(|(index, _)| index).chain(std::iter::once(source.len()))
            {
                pending.push_str(&source[start..end]);
                start = end;
                let consumed = {
                    let mut events = parser.feed(&pending);
                    for event in events.by_ref() {
                        streaming.push_event(&event);
                    }
                    events.consumed()
                };
                pending.drain(..consumed);
            }
            for event in parser.finish_iter(&pending) {
                streaming.push_event(&event);
            }
            assert_eq!(streaming.into_string(), complete.into_string(), "options: {options:?}, chunk_chars: {chunk_chars}");
        }
        true
    }

    QuickCheck::new()
        .tests(20_000)
        .max_tests(20_000)
        .quickcheck(property as fn(String, u16, u8) -> bool);
}
