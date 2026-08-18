const DISALLOWED: &[&str] = &[
    "title",
    "textarea",
    "style",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "script",
    "plaintext",
];

pub fn filter_disallowed_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    filter_disallowed_html_into(&mut output, input);
    output
}

/// Copies bytes from `input` to the `output`, applying the tagfilter on the fly.
pub fn filter_disallowed_html_into(output: &mut String, input: &str) {
    let bytes = input.as_bytes();
    let mut copied = 0;
    let mut pos = 0;

    while pos < bytes.len() {
        if bytes[pos] != b'<' {
            pos += 1;
            continue;
        }

        let mut name_start = pos + 1;
        if bytes.get(name_start) == Some(&b'/') {
            name_start += 1;
        }
        let mut name_end = name_start;
        while bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        {
            name_end += 1;
        }

        let boundary = bytes.get(name_end).copied();
        let valid_boundary = boundary.is_some_and(|byte| {
            byte.is_ascii_whitespace()
                || byte == b'>'
                || (byte == b'/' && bytes.get(name_end + 1) == Some(&b'>'))
        });

        // Check if the HTML tag is disallowed, and filter if so.
        let disallowed = valid_boundary
            && DISALLOWED
                .iter()
                .any(|tag| input[name_start..name_end].eq_ignore_ascii_case(tag));

        if disallowed {
            output.push_str(&input[copied..pos]);
            output.push_str("&lt;");
            copied = pos + 1;
        }
        pos += 1;
    }

    output.push_str(&input[copied..]);
}
