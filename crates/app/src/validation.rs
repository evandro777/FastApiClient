//! Advisory checks only: never rewrite the draft or change whether a request can be sent.
use crate::Validation;
use fastapiclient_core::http::small_text;

// kind: 0 pending, 1 valid, 2 parser error, 3 template, 4 DTD, 5 comment, 6/7 newline, 8 tab, 9 control, 10 printable, 11 size.
pub fn check(text: &str, body_type: i32) -> Validation {
    let result = |kind, detail: String, line, column| Validation {
        kind,
        detail: detail.into(),
        line,
        column,
        ..Default::default()
    };
    if matches!(body_type, 3 | 4) && !small_text(text) {
        return result(11, String::new(), 0, 0);
    }
    // Template contents need not follow JSON quoting or comment syntax, even while incomplete.
    if matches!(body_type, 3 | 4) && text.contains("{{") {
        return result(3, String::new(), 0, 0);
    }
    let json = if body_type == 3 {
        match json_without_comments(text) {
            Ok(json) => Some(json),
            Err(offset) => {
                return result(5, String::new(), location(text, offset).0, location(text, offset).1);
            }
        }
    } else {
        None
    };
    let input = json.as_deref().unwrap_or(text);
    let error = if body_type == 3 {
        match serde_json::from_str::<serde_json::Value>(input) {
            Ok(_) => None,
            Err(e) => return json_error(text, input, e),
        }
    } else if body_type == 4 {
        match roxmltree::Document::parse(input) {
            Err(roxmltree::Error::DtdDetected) => return result(4, String::new(), 0, 0),
            Err(e) => Some(e.to_string()),
            Ok(_) => None,
        }
    } else {
        return Validation::default();
    };
    match error {
        Some(error) => result(2, error, 0, 0),
        None => result(1, String::new(), 0, 0),
    }
}

/// Mask rather than delete comments so parser locations still refer to the draft.
pub(crate) fn json_without_comments(text: &str) -> Result<String, usize> {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let (mut i, mut quoted) = (0, false);
    while i < bytes.len() {
        if quoted {
            match bytes[i] {
                b'\\' => i += 1,
                b'"' => quoted = false,
                _ => {}
            }
        } else if bytes[i] == b'"' {
            quoted = true;
        } else if bytes[i..].starts_with(b"//") || bytes[i..].starts_with(b"/*") {
            let start = i;
            let block = bytes[i + 1] == b'*';
            i += 2;
            if block {
                while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                    i += 1;
                }
                if i == bytes.len() {
                    return Err(start);
                }
                i += 2;
            } else {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            for byte in &mut out[start..i] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
            continue;
        }
        i += 1;
    }
    Ok(String::from_utf8(out).expect("comments were replaced with ASCII whitespace"))
}

fn location(text: &str, offset: usize) -> (i32, i32) {
    let prefix = &text[..offset];
    (
        prefix.bytes().filter(|&b| b == b'\n').count() as i32 + 1,
        prefix.rsplit('\n').next().unwrap_or("").chars().count() as i32 + 1,
    )
}

fn json_error(text: &str, masked: &str, error: serde_json::Error) -> Validation {
    let start = masked
        .split_inclusive('\n')
        .take(error.line().saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let offset = start + error.column().saturating_sub(1);
    let diagnostic = |kind, offset, character: String| {
        let (line, column) = location(text, offset);
        Validation {
            kind,
            line,
            column,
            character: character.into(),
            detail: error.to_string().into(),
        }
    };
    // Scan the masked source: serde reports a range for controls, and a newline's position can be on the next line.
    let (mut quoted, mut escaped) = (false, false);
    for (offset, ch) in masked.char_indices() {
        if offset > start + error.column() {
            break;
        }
        if quoted && ch <= '\u{1f}' {
            let kind = match ch {
                '\n' | '\r' if text[..offset].ends_with(['“', '”']) => 7,
                '\n' | '\r' => 6,
                '\t' => 8,
                _ => 9,
            };
            let code = format!("U+{:04X}", ch as u32);
            let character = match ch {
                '\n' => format!("↵, \\n, {code}"),
                '\r' => format!("↵, \\r, {code}"),
                '\t' => format!("⇥, \\t, {code}"),
                _ => code,
            };
            return diagnostic(kind, offset, character);
        }
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        }
    }
    if let Some(ch) = text
        .get(offset..)
        .and_then(|s| s.chars().next())
        .filter(|c| !error.is_eof() && !c.is_whitespace() && !c.is_control())
    {
        return diagnostic(10, offset, format!("{ch} (U+{:04X})", ch as u32));
    }
    Validation {
        kind: 2,
        detail: error.to_string().into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_payloads_skip_automatic_parsing() {
        use fastapiclient_core::http::AUTO_TEXT_LIMIT;
        for (kind, body) in [(3, "[0]"), (4, "<r/>")] {
            for size in [AUTO_TEXT_LIMIT - 1, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT + 1] {
                let source = format!("{body}{}", " ".repeat(size - body.len()));
                assert_eq!(check(&source, kind).kind, if size <= AUTO_TEXT_LIMIT { 1 } else { 11 });
            }
        }
        assert_eq!(check(&"猫".repeat(AUTO_TEXT_LIMIT / 3 + 1), 3).kind, 11);
    }

    #[test]
    fn json_checks_syntax_and_accepts_comments_without_losing_locations() {
        for text in [
            r#"{"url":"https://example.com/*x*/","quote":"\""}"#,
            "[1, true, null]",
            "42",
        ] {
            assert_eq!(check(text, 3).kind, 1, "{text}");
        }
        assert_eq!(check("{/* 猫 */\n// note\n\"a\":1}", 3).kind, 1);
        let error = check("{/* 猫 */\n\"a\": }", 3);
        assert_eq!(error.kind, 10);
        assert!(error.detail.contains("line 2 column 6"));
        assert_eq!(json_without_comments("/*é*/\r\n{}").unwrap(), "      \r\n{}");
        let error = check("{\n  /* unfinished", 3);
        assert_eq!((error.kind, error.line, error.column), (5, 2, 3));
        for text in ["", "  \n", "{} {}", "{\"a\":1,}"] {
            assert!(matches!(check(text, 3).kind, 2 | 10), "{text}");
        }
    }

    #[test]
    fn json_diagnostics_name_actual_characters_and_use_source_columns() {
        let error = check("{\n    \"name\": \"Ana”\n}", 3);
        assert_eq!((error.kind, error.line, error.column), (7, 2, 18));
        assert_eq!(error.character, "↵, \\n, U+000A");
        for (ch, kind, code) in [
            ('\n', 6, "↵, \\n, U+000A"),
            ('\t', 8, "⇥, \\t, U+0009"),
            ('\u{1}', 9, "U+0001"),
            ('\r', 6, "↵, \\r, U+000D"),
        ] {
            let error = check(&format!("\"é猫{ch}x\""), 3);
            assert_eq!((error.kind, error.line, error.column), (kind, 1, 4));
            assert_eq!(error.character, code);
        }
        assert_eq!(check("\"Ana”\r\n", 3).kind, 7);
        let error = check("{\"é猫\": @}", 3);
        assert_eq!((error.kind, error.line, error.column), (10, 1, 8));
        assert_eq!(error.character, "@ (U+0040)");
        assert_eq!(check("{\"x\": ”}", 3).character, "” (U+201D)");
        assert_eq!(check("{/*é*/\"x\": @}", 3).column, 12);
        // Report the first parser failure, not an unrelated later control character.
        assert_eq!(check("{x: \"a\nb\"}", 3).character, "x (U+0078)");
        for source in ["", "{", "[1", "{\"a\":", "{\"a\": \"unfinished"] {
            let result = check(source, 3);
            assert_eq!(result.kind, 2);
            assert_eq!(
                result.detail,
                serde_json::from_str::<serde_json::Value>(source)
                    .unwrap_err()
                    .to_string()
            );
        }
    }

    #[test]
    fn templates_are_deferred_without_resolution() {
        for (text, kind) in [
            ("{\"a\": {{count}}}", 3),
            ("{{", 3),
            ("{{ $response('request', '$.a/*') }}", 3),
            ("<a>{{value}}</a>", 4),
        ] {
            assert_eq!(check(text, kind).kind, 3);
        }
    }

    #[test]
    fn xml_checks_well_formedness_without_loading_dtds() {
        for text in ["<a/>", "<?xml version=\"1.0\"?><a x=\"1\"><![CDATA[x]]>&amp;</a>"] {
            assert_eq!(check(text, 4).kind, 1);
        }
        for text in ["", " \n", "<a></b>", "<a/><b/>", "<a x=1/>", "<a>&unknown;</a>"] {
            assert_eq!(check(text, 4).kind, 2, "{text}");
        }
        let error = check("<a>\n</b>", 4);
        assert!(error.detail.contains("2:1"));
        assert_eq!(check("<!DOCTYPE a SYSTEM 'file:///tmp/unused'><a/>", 4).kind, 4);
        assert_eq!(check("", 0).kind, 0);
    }
}
