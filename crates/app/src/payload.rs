//! Request-only whitespace formatting. Keep source tokens; serializing a parsed tree changes values.
use crate::validation::json_without_comments;
use fastapiclient_core::http::{AUTO_TEXT_LIMIT, small_text};

pub fn format(text: &str, kind: i32) -> String {
    if !small_text(text) || text.contains("{{") {
        return text.into();
    }
    match kind {
        3 => json(text),
        4 => xml(text),
        _ => None,
    }
    .unwrap_or_else(|| text.into())
}

fn newline(out: &mut String, depth: usize) -> Option<()> {
    if out.len() + 1 + depth * 4 > AUTO_TEXT_LIMIT {
        return None;
    }
    while out.ends_with([' ', '\t']) {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for _ in 0..depth {
        out.push_str("    ");
    }
    Some(())
}

fn json(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&json_without_comments(text).ok()?).ok()?;
    let bytes = text.as_bytes();
    let (mut i, mut depth) = (0, 0usize);
    let mut tokens = vec![];
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while !bytes[i..].starts_with(b"*/") {
                    i += 1;
                }
                i += 2;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => i += 1,
            _ => {
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !b"{}[],:/".contains(&bytes[i]) {
                    i += 1;
                }
            }
        }
        tokens.push(&text[start..i]);
    }
    let mut out = String::with_capacity(text.len());
    for (i, token) in tokens.iter().enumerate() {
        if !small_text(&out) {
            return None;
        }
        let prev = i.checked_sub(1).map(|i| tokens[i]).unwrap_or("");
        match *token {
            "{" | "[" => {
                out.push_str(token);
                depth += 1;
                if !matches!(tokens.get(i + 1), Some(&"}") | Some(&"]")) {
                    newline(&mut out, depth)?;
                }
            }
            "}" | "]" => {
                depth -= 1;
                if !matches!(prev, "{" | "[") {
                    newline(&mut out, depth)?;
                }
                out.push_str(token);
            }
            "," => {
                out.push(',');
                newline(&mut out, depth)?;
            }
            ":" => out.push_str(": "),
            _ if token.starts_with("//") => {
                out.push_str(token);
                if i + 1 < tokens.len() {
                    out.push('\n');
                    for _ in 0..depth {
                        out.push_str("    ");
                    }
                }
            }
            _ if token.starts_with("/*") => {
                out.push_str(token);
                if i + 1 < tokens.len() {
                    out.push(' ');
                }
            }
            _ => out.push_str(token),
        }
    }
    small_text(&out).then_some(out)
}

/// Re-indenting must not change what the document says: refuse CDATA, `xml:space="preserve"`, text mixed with
/// elements, and whitespace written as references (they would vanish when indenting an element-only parent).
/// Shared by the request Format button and the response beautifier.
pub(crate) fn reindent_is_safe(doc: &roxmltree::Document, text: &str) -> bool {
    !text.contains("<![CDATA[")
        && !doc.descendants().any(|node| {
            (node.is_text() && node.text().unwrap_or("").trim().is_empty() && text[node.range()].contains('&'))
                || node.is_element()
                    && (node.attribute(("http://www.w3.org/XML/1998/namespace", "space")) == Some("preserve")
                        || (node.children().any(|n| !n.is_text())
                            && node
                                .children()
                                .any(|n| n.is_text() && !n.text().unwrap_or("").trim().is_empty())))
        })
}

fn xml(text: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(text).ok()?;
    if !reindent_is_safe(&doc, text) {
        return None;
    }
    fn render(node: roxmltree::Node<'_, '_>, text: &str, out: &mut String) -> Option<()> {
        // Use a work list so deeply nested input cannot exhaust the UI thread's stack.
        let mut pending = vec![(node, 0, false)];
        let mut first = true;
        while let Some((node, depth, closing)) = pending.pop() {
            if !small_text(out) {
                return None;
            }
            let source = &text[node.range()];
            if !first {
                newline(out, depth)?;
            }
            first = false;
            if closing {
                out.push_str(&source[source.rfind("</").expect("element with children has an end tag")..]);
                continue;
            }
            if !node.is_element() || !node.children().any(|n| !n.is_text()) {
                out.push_str(source);
                continue;
            }
            // Find the opening tag's end without mistaking a quoted '>' for it.
            let mut quote = None;
            let end = source
                .char_indices()
                .find_map(|(i, ch)| {
                    if quote == Some(ch) {
                        quote = None;
                    } else if quote.is_none() {
                        if matches!(ch, '\'' | '"') {
                            quote = Some(ch);
                        } else if ch == '>' {
                            return Some(i + 1);
                        }
                    }
                    None
                })
                .expect("parsed element has an opening tag");
            out.push_str(&source[..end]);
            pending.push((node, depth, true));
            pending.extend(
                node.children()
                    .rev()
                    .filter(|n| !n.is_text())
                    .map(|n| (n, depth + 1, false)),
            );
        }
        Some(())
    }
    let mut out = String::with_capacity(text.len());
    let mut end = 0;
    for node in doc.root().children().filter(|n| !n.is_text()) {
        let prefix = text[end..node.range().start].trim();
        if !prefix.is_empty() {
            out.push_str(prefix);
            newline(&mut out, 0)?;
        } else if !out.is_empty() {
            newline(&mut out, 0)?;
        }
        render(node, text, &mut out)?;
        end = node.range().end;
    }
    small_text(&out).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_formatting_uses_the_shared_byte_limit_and_bounds_expansion() {
        for size in [AUTO_TEXT_LIMIT - 1, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT + 1] {
            for (kind, body) in [(3, "[0]"), (4, "<r><a/></r>")] {
                let text = format!("{body}{}", " ".repeat(size - body.len()));
                assert_eq!(format(&text, kind) != text, size <= AUTO_TEXT_LIMIT);
            }
        }
        for (kind, text) in [
            (3, format!("[{}0]", "0,".repeat(12_000))),
            (4, format!("{}{}", "<r>".repeat(300), "</r>".repeat(300))),
        ] {
            assert!(small_text(&text));
            assert_eq!(format(&text, kind), text);
        }
    }

    #[test]
    fn json_keeps_order_numbers_strings_and_comments() {
        let source = r#"{"z":1e+02,"a":[-0,1.2300,"a\n\u0061"],/*猫*/"z":2}//end"#;
        let pretty = format(source, 3);
        assert_eq!(
            pretty,
            "{\n    \"z\": 1e+02,\n    \"a\": [\n        -0,\n        1.2300,\n        \"a\\n\\u0061\"\n    ],\n    /*猫*/ \"z\": 2\n}//end"
        );
        assert!(serde_json::from_str::<serde_json::Value>(&json_without_comments(&pretty).unwrap()).is_ok());
        assert_eq!(format(&pretty, 3), pretty);
        for source in ["{}", "[]", "42", "// note\n{}", "{/* a\nb */}", "[1,// tail\n2]"] {
            let pretty = format(source, 3);
            assert!(
                serde_json::from_str::<serde_json::Value>(&json_without_comments(&pretty).unwrap()).is_ok(),
                "{pretty}"
            );
        }
        assert_eq!(
            format("[1,// keep these spaces  \n2]// tail  ", 3),
            "[\n    1,\n    // keep these spaces  \n    2\n]// tail  "
        );
    }

    #[test]
    fn xml_preserves_leaf_text_tags_entities_and_declaration() {
        let source = "<?xml version='1.0'?><root a='>'><x> A &amp; B </x><!--hi--><n/><n> </n></root>";
        let pretty = format(source, 4);
        assert_eq!(
            pretty,
            "<?xml version='1.0'?>\n<root a='>'>\n    <x> A &amp; B </x>\n    <!--hi-->\n    <n/>\n    <n> </n>\n</root>"
        );
        assert_eq!(format(&pretty, 4), pretty);
        assert_eq!(
            format("<a><b><c/></b><d/></a>", 4),
            "<a>\n    <b>\n        <c/>\n    </b>\n    <d/>\n</a>"
        );
    }

    #[test]
    fn unsafe_or_invalid_documents_stay_verbatim() {
        for source in ["{\"x\":}", "{/* unfinished", "{\"x\":\"{{v}}\"}"] {
            assert_eq!(format(source, 3), source);
        }
        for source in [
            "<a>",
            "<!DOCTYPE a><a/>",
            "<p>hi <b>x</b>!</p>",
            "<a xml:space='preserve'><b/></a>",
            "<a><b>{{v}}</b></a>",
            "<a><![CDATA[ ]]><b/></a>",
            "<a>&#32;<b/></a>",
        ] {
            assert_eq!(format(source, 4), source);
        }
    }
}
