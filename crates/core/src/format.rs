//! Text formatting that has nothing to do with the network: re-indenting JSON/XML responses and stripping comments
//! from JSON request bodies. Shared size limit for automatic formatting and coloring.
use serde_json::Value;

/// Automatic formatting and syntax coloring share this UTF-8 byte limit.
pub const AUTO_TEXT_LIMIT: usize = 64 * 1024;

pub fn small_text(text: &str) -> bool {
    text.len() <= AUTO_TEXT_LIMIT
}

/// Re-indents a JSON object/array and decodes Unicode escapes for display. Works on the text so key order and number
/// digits stay exactly as the server sent them. `None` when it is not valid JSON or not an object/array.
pub fn pretty_json(s: &str) -> Option<String> {
    if !s.trim_start().starts_with(['{', '[']) {
        return None;
    }
    pretty_json_limited(s, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT).ok()
}

#[derive(Debug, PartialEq)]
pub enum FormatError {
    Invalid,
    Limit,
}

pub fn pretty_json_limited(s: &str, input_limit: usize, output_limit: usize) -> Result<String, FormatError> {
    if s.len() > input_limit {
        return Err(FormatError::Limit);
    }
    let t = s.trim();
    if serde_json::from_str::<Value>(t).is_err() {
        return Err(FormatError::Invalid);
    }
    let mut out = String::with_capacity(t.len() * 2);
    let mut depth = 0usize;
    let mut it = t.chars().peekable();
    let nl = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = it.next() {
        if out.len() > output_limit {
            return Err(FormatError::Limit);
        }
        match c {
            '"' => {
                let mut token = String::from("\"");
                let mut unicode = false;
                while let Some(c) = it.next() {
                    token.push(c);
                    if c == '\\' {
                        let escaped = it.next().ok_or(FormatError::Invalid)?;
                        unicode |= escaped == 'u';
                        token.push(escaped);
                    } else if c == '"' {
                        break;
                    }
                }
                if unicode {
                    let decoded: String = serde_json::from_str(&token).map_err(|_| FormatError::Invalid)?;
                    // Re-escape quotes and controls so the displayed body remains valid JSON.
                    out.push_str(&serde_json::to_string(&decoded).map_err(|_| FormatError::Invalid)?);
                } else {
                    out.push_str(&token);
                }
            }
            '{' | '[' => {
                out.push(c);
                // Valid JSON, so whitespace is the only thing that can sit between an opener and its closer.
                while it.next_if(|w| w.is_whitespace()).is_some() {}
                if it.peek().is_some_and(|n| matches!(n, '}' | ']')) {
                    out.extend(it.next());
                } else {
                    depth += 1;
                    nl(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth -= 1;
                nl(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                nl(&mut out, depth);
            }
            ':' => out.push_str(": "),
            w if w.is_whitespace() => {}
            _ => out.push(c),
        }
    }
    (out.len() <= output_limit).then_some(out).ok_or(FormatError::Limit)
}

enum Xml<'a> {
    Open(&'a str),
    Close(&'a str),
    /// Self-closing tags, declarations, comments, CDATA and DOCTYPE: one line each, no nesting.
    Leaf(&'a str),
    Text(&'a str),
}

/// Splits XML into tags and verbatim text runs; `None` when a tag, comment or CDATA is not terminated.
/// shortcut: a DOCTYPE internal subset containing '>' is not supported, add it if a real API sends one.
fn xml_tokens(t: &str) -> Option<Vec<Xml<'_>>> {
    let mut out = vec![];
    let mut i = 0;
    while i < t.len() {
        let rest = &t[i..];
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = &rest[..end];
            if !text.is_empty() {
                out.push(Xml::Text(text));
            }
            i += end;
            continue;
        }
        let end = if rest.starts_with("<!--") {
            rest.find("-->")? + 3
        } else if rest.starts_with("<![CDATA[") {
            rest.find("]]>")? + 3
        } else if rest.starts_with("<?") {
            rest.find("?>")? + 2
        } else {
            // The first '>' outside quotes: an attribute value may contain one.
            let mut quote = None;
            let mut found = None;
            for (n, c) in rest.char_indices() {
                match (quote, c) {
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(q), c) if c == q => quote = None,
                    (None, '>') => {
                        found = Some(n + 1);
                        break;
                    }
                    _ => {}
                }
            }
            found?
        };
        let tag = &rest[..end];
        out.push(if tag.starts_with("</") {
            Xml::Close(tag)
        } else if tag.starts_with("<!") || tag.starts_with("<?") || tag.ends_with("/>") {
            Xml::Leaf(tag)
        } else {
            Xml::Open(tag)
        });
        i += end;
    }
    Some(out)
}

fn xml_name(tag: &str) -> &str {
    let name = tag.trim_start_matches(['<', '/']);
    name.split(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .next()
        .unwrap_or("")
}

/// Re-indents XML for display, working on the text so attributes, namespaces and entities stay as sent. `None` when it
/// is not well-formed (mismatched or unclosed tags, no element), so HTML or a truncated body stays as it is.
pub fn pretty_xml(s: &str) -> Option<String> {
    pretty_xml_limited(s, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT).ok()
}

pub fn pretty_xml_limited(s: &str, input_limit: usize, output_limit: usize) -> Result<String, FormatError> {
    if s.len() > input_limit {
        return Err(FormatError::Limit);
    }
    let t = s.trim();
    if !t.starts_with('<') {
        return Err(FormatError::Invalid);
    }
    let tokens = xml_tokens(t).ok_or(FormatError::Invalid)?;
    let mut out = String::with_capacity(t.len() * 2);
    let mut stack: Vec<&str> = vec![];
    let mut elements = 0;
    let mut i = 0;
    let line = |out: &mut String, depth: usize, parts: &[&str]| -> Result<(), FormatError> {
        if out.len() + usize::from(!out.is_empty()) + depth * 2 + parts.iter().map(|p| p.len()).sum::<usize>()
            > output_limit
        {
            return Err(FormatError::Limit);
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&"  ".repeat(depth));
        parts.iter().for_each(|p| out.push_str(p));
        Ok(())
    };
    while i < tokens.len() {
        match &tokens[i] {
            Xml::Open(tag) => {
                elements += 1;
                let name = xml_name(tag);
                // `<n>text</n>` and `<n></n>` stay on one line.
                match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(Xml::Text(text)), Some(Xml::Close(c))) if xml_name(c) == name => {
                        line(&mut out, stack.len(), &[tag, text, c])?;
                        i += 3;
                        continue;
                    }
                    (Some(Xml::Close(c)), _) if xml_name(c) == name => {
                        line(&mut out, stack.len(), &[tag, c])?;
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
                line(&mut out, stack.len(), &[tag])?;
                stack.push(name);
            }
            Xml::Close(tag) => {
                if stack.pop() != Some(xml_name(tag)) {
                    return Err(FormatError::Invalid);
                }
                line(&mut out, stack.len(), &[tag])?;
            }
            Xml::Leaf(tag) => {
                elements += usize::from(!tag.starts_with("<!") && !tag.starts_with("<?"));
                line(&mut out, stack.len(), &[tag])?;
            }
            Xml::Text(text) => {
                if text.trim().is_empty() {
                    i += 1;
                    continue;
                }
                if stack.is_empty() {
                    return Err(FormatError::Invalid);
                }
                line(&mut out, stack.len(), &[text])?;
            }
        }
        i += 1;
    }
    (stack.is_empty() && elements > 0)
        .then_some(out)
        .ok_or(FormatError::Invalid)
}

/// Removes `//` and `/* */` comments outside strings. Trailing commas are not handled: shortcut, add it if users
/// paste JSON5-style bodies.
pub(crate) fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.char_indices().peekable();
    let mut in_str = false;
    while let Some((offset, c)) = it.next() {
        // Template arguments have their own quoting rules, including literal comment markers.
        if s[offset..].starts_with("{{") {
            let end = crate::resolve::expression_end(&s[offset + 2..]).map_or(s.len(), |len| offset + len + 4);
            out.push_str(&s[offset..end]);
            while it.next_if(|(i, _)| *i < end).is_some() {}
            continue;
        }
        if in_str {
            out.push(c);
            if c == '\\' {
                out.extend(it.next().map(|(_, ch)| ch));
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, it.peek().map(|(_, ch)| *ch)) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => while it.next_if(|&(_, ch)| ch != '\n').is_some() {},
            ('/', Some('*')) => {
                it.next();
                let mut prev = ' ';
                for (_, n) in it.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}
