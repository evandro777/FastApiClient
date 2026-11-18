//! Moves the selected node between the model and the editor widgets: `load` on selection, `store` when the user
//! has edited something.
use fastapiclient_core::{
    model::{Auth, Basic, Body, KeyValue, OAuth2, Request},
    workspace::Node,
};
use slint::{Model, SharedString, VecModel};
use std::rc::Rc;

use crate::{CodeChange, KeyValueRow, Main, Tables};

pub const STANDARD_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// The tables always end with one blank row, so there is somewhere to type the next entry.
pub fn ensure_blank(table: &VecModel<KeyValueRow>) {
    let blank = |r: &KeyValueRow| r.key.is_empty() && r.value.is_empty() && r.description.is_empty();
    if table.row_count() == 0 || !table.row_data(table.row_count() - 1).is_some_and(|r| blank(&r)) {
        table.push(KeyValueRow {
            enabled: true,
            ..Default::default()
        });
    }
}

fn set_rows(table: &VecModel<KeyValueRow>, list: &[KeyValue]) {
    table.set_vec(
        list.iter()
            .map(|kv| KeyValueRow {
                enabled: kv.enabled,
                key: kv.key.as_str().into(),
                value: kv.value.as_str().into(),
                description: kv.description.as_str().into(),
            })
            .collect::<Vec<_>>(),
    );
    ensure_blank(table);
}

/// Blank rows are the editing placeholder, never data.
fn rows(table: &VecModel<KeyValueRow>) -> Vec<KeyValue> {
    table
        .iter()
        .filter(|r| !(r.key.is_empty() && r.value.is_empty() && r.description.is_empty()))
        .map(|r| KeyValue {
            key: r.key.into(),
            value: r.value.into(),
            description: r.description.into(),
            enabled: r.enabled,
        })
        .collect()
}

fn load_auth(ui: &Main, auth: &Auth) {
    let (kind, user, pass, token) = match auth {
        Auth::Inherit => (0, "", "", ""),
        Auth::None => (1, "", "", ""),
        Auth::Basic(b) => (2, b.username.as_str(), b.password.as_str(), ""),
        Auth::Bearer(t) => (3, "", "", t.as_str()),
        Auth::OAuth2(_) => (4, "", "", ""),
    };
    ui.set_auth_type(kind);
    ui.set_auth_username(user.into());
    ui.set_auth_password(pass.into());
    ui.set_auth_token(token.into());
}

/// `current` is what is stored now: OAuth 2.0 has no editor yet, so it must survive a round trip untouched.
fn read_auth(ui: &Main, current: &Auth) -> Auth {
    match ui.get_auth_type() {
        1 => Auth::None,
        2 => Auth::Basic(Basic {
            username: ui.get_auth_username().into(),
            password: ui.get_auth_password().into(),
        }),
        3 => Auth::Bearer(ui.get_auth_token().into()),
        4 => match current {
            Auth::OAuth2(o) => Auth::OAuth2(o.clone()),
            _ => Auth::OAuth2(OAuth2::default()),
        },
        _ => Auth::Inherit,
    }
}

fn load_body(ui: &Main, t: &Tables, body: &Body) {
    let (kind, text, file, form): (i32, &str, &str, &[KeyValue]) = match body {
        Body::None => (0, "", "", &[]),
        Body::FormData(kv) => (1, "", "", kv),
        Body::FormUrl(kv) => (2, "", "", kv),
        Body::Json(s) => (3, s, "", &[]),
        Body::Xml(s) => (4, s, "", &[]),
        Body::File(p) => (5, "", p, &[]),
    };
    ui.set_body_type(kind);
    ui.set_body_text(text.into());
    ui.set_body_file(file.into());
    set_rows(&t[2], form);
}

fn read_body(ui: &Main, t: &Tables) -> Body {
    match ui.get_body_type() {
        1 => Body::FormData(rows(&t[2])),
        2 => Body::FormUrl(rows(&t[2])),
        3 => Body::Json(ui.get_body_text().into()),
        4 => Body::Xml(ui.get_body_text().into()),
        5 => Body::File(ui.get_body_file().trim().into()),
        _ => Body::None,
    }
}

/// `methods` is the ComboBox model: the standard verbs, plus the request's own when it is a custom one.
pub fn load(ui: &Main, t: &Tables, methods: &VecModel<SharedString>, node: Node<'_>) {
    match node {
        Node::Project(p) => {
            set_rows(&t[1], &p.headers);
            load_auth(ui, &p.auth);
        }
        Node::Folder(f) => {
            set_rows(&t[1], &f.headers);
            load_auth(ui, &f.auth);
        }
        Node::Request(r) => {
            let mut list: Vec<SharedString> = STANDARD_METHODS.iter().map(|m| (*m).into()).collect();
            let index = STANDARD_METHODS.iter().position(|m| *m == r.method).unwrap_or_else(|| {
                list.push(r.method.as_str().into());
                list.len() - 1
            });
            methods.set_vec(list);
            ui.set_method_index(index as i32);
            ui.set_url(r.url.as_str().into());
            set_rows(&t[0], &r.query);
            set_rows(&t[1], &r.headers);
            load_auth(ui, &r.auth);
            load_body(ui, t, &r.body);
        }
    }
}

/// Copies the widgets into the node; true when that changed it, so the caller knows to save.
pub fn store(ui: &Main, t: &Tables, methods: &VecModel<SharedString>, node: Node<'_>) -> bool {
    match node {
        Node::Project(p) => {
            let (headers, auth) = (rows(&t[1]), read_auth(ui, &p.auth));
            let changed = p.headers != headers || p.auth != auth;
            (p.headers, p.auth) = (headers, auth);
            changed
        }
        Node::Folder(f) => {
            let (headers, auth) = (rows(&t[1]), read_auth(ui, &f.auth));
            let changed = f.headers != headers || f.auth != auth;
            (f.headers, f.auth) = (headers, auth);
            changed
        }
        Node::Request(r) => {
            let method = methods
                .row_data(ui.get_method_index().max(0) as usize)
                .map_or_else(|| r.method.clone(), |m| m.to_string());
            let new = Request {
                name: r.name.clone(),
                method,
                url: ui.get_url().trim().into(),
                query: rows(&t[0]),
                headers: rows(&t[1]),
                auth: read_auth(ui, &r.auth),
                body: read_body(ui, t),
            };
            let changed = *r != new;
            *r = new;
            changed
        }
    }
}

pub type Methods = Rc<VecModel<SharedString>>;

/// What Tab inserts in the JSON, XML and environment editors.
pub const INDENT: &str = "    ";

/// Offsets are UTF-8 byte positions, as used by Slint. Keep selection direction when editing whole lines.
pub fn indent(text: &str, anchor: usize, caret: usize, unindent: bool) -> CodeChange {
    let (anchor, caret) = (snap(text, anchor), snap(text, caret));
    if anchor == caret && !unindent {
        let mut out = text.to_owned();
        out.insert_str(caret, INDENT);
        return CodeChange {
            text: out.into(),
            anchor: (caret + INDENT.len()) as i32,
            caret: (caret + INDENT.len()) as i32,
        };
    }
    let (start, end) = (anchor.min(caret), anchor.max(caret));
    let first = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let mut edits = vec![];
    let mut line = first;
    loop {
        let removed = if !unindent {
            0
        } else if text[line..].starts_with('\t') {
            1
        } else {
            text[line..]
                .bytes()
                .take(INDENT.len())
                .take_while(|&b| b == b' ')
                .count()
        };
        edits.push((line, removed));
        match text[line..].find('\n').map(|i| line + i + 1) {
            Some(next) if next < end => line = next,
            _ => break,
        }
    }
    let adjusted = |position: usize| {
        let mut result = position;
        for &(line, removed) in &edits {
            if line <= position {
                if unindent {
                    result -= removed.min(position - line);
                } else {
                    result += INDENT.len();
                }
            }
        }
        result as i32
    };
    let mut out = String::with_capacity(text.len() + if unindent { 0 } else { edits.len() * INDENT.len() });
    let mut copied = 0;
    for &(line, removed) in &edits {
        out.push_str(&text[copied..line]);
        if !unindent {
            out.push_str(INDENT);
        }
        copied = line + removed;
    }
    out.push_str(&text[copied..]);
    CodeChange {
        text: out.into(),
        anchor: adjusted(anchor),
        caret: adjusted(caret),
    }
}

fn snap(text: &str, i: usize) -> usize {
    (0..=i.min(text.len()))
        .rev()
        .find(|&j| text.is_char_boundary(j))
        .unwrap_or(0)
}

pub fn line_numbers(text: &str) -> String {
    (1..=text.bytes().filter(|&b| b == b'\n').count() + 1)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Enter replaces the selection, preserving its line's indent. Only request editors use this.
pub fn enter(text: &str, anchor: usize, caret: usize, kind: i32) -> CodeChange {
    let (start, end) = (snap(text, anchor.min(caret)), snap(text, anchor.max(caret)));
    let line = text[..start].rsplit('\n').next().unwrap_or("");
    let indent = &line[..line.bytes().take_while(|b| matches!(b, b' ' | b'\t')).count()];
    let before = text[..start].trim_end_matches([' ', '\t']);
    let after = text[end..].trim_start_matches([' ', '\t']);
    let opening = if kind == 3 {
        before.ends_with(['{', '[']) && json_outside_string(before)
    } else {
        xml_opening(before).is_some()
    };
    let matching = if kind == 3 {
        (before.ends_with('{') && after.starts_with('}')) || (before.ends_with('[') && after.starts_with(']'))
    } else {
        xml_opening(before).is_some_and(|name| {
            after.strip_prefix("</").is_some_and(|tail| {
                tail.strip_prefix(name)
                    .is_some_and(|s| s.starts_with(['>', ' ', '\t', '\r', '\n']))
            })
        })
    };
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let extra = if opening { INDENT } else { "" };
    let mut out = format!("{}{eol}{indent}{extra}", &text[..start]);
    let position = out.len() as i32;
    if opening && matching {
        out.push_str(&format!("{eol}{indent}"));
    }
    out.push_str(if opening && matching { after } else { &text[end..] });
    CodeChange {
        text: out.into(),
        anchor: position,
        caret: position,
    }
}

fn json_outside_string(text: &str) -> bool {
    let Ok(text) = crate::validation::json_without_comments(text) else {
        return false;
    };
    // An opening delimiter in a comment is masked out.
    if !text.trim_end().ends_with(['{', '[']) {
        return false;
    }
    let (mut quoted, mut escaped) = (false, false);
    for ch in text.chars() {
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        }
    }
    !quoted
}

fn xml_opening(text: &str) -> Option<&str> {
    for (open, close) in [("<!--", "-->"), ("<![CDATA[", "]]>")] {
        if text
            .rfind(open)
            .is_some_and(|start| text.rfind(close).is_none_or(|end| start > end))
        {
            return None;
        }
    }
    let (_, tag) = text.strip_suffix('>')?.rsplit_once('<')?;
    if tag.starts_with(['/', '!', '?']) || tag.ends_with('/') {
        return None;
    }
    let name = tag.split_whitespace().next()?;
    if name.contains(['\'', '"', '<', '>']) {
        return None;
    }
    let mut quote = None;
    for ch in tag.chars() {
        if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() {
            if matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if ch == '>' {
                return None;
            }
        }
    }
    quote.is_none().then_some(name)
}

/// Where an unfinished `{{partial` ends at `caret`: its start byte and the partial name typed so far.
fn open_var(text: &str, caret: usize) -> Option<(usize, &str)> {
    let caret = snap(text, caret);
    let start = text[..caret].rfind("{{")?;
    let partial = &text[start + 2..caret];
    (!partial.contains('}')).then_some((start, partial.trim_start()))
}

/// Names to offer at `caret`: those containing what follows an open `{{`; with none open, every name when `all`
/// (Ctrl+Space) and nothing otherwise.
pub fn var_matches(names: &[String], text: &str, caret: usize, all: bool) -> Vec<String> {
    match open_var(text, caret) {
        Some((_, p)) => {
            let p = p.to_lowercase();
            names
                .iter()
                .filter(|n| n.to_lowercase().contains(&p))
                .cloned()
                .collect()
        }
        None if all => names.to_vec(),
        None => vec![],
    }
}

/// `text` with `{{name}}` replacing the open `{{partial` before `caret`, or inserted at `caret` when none is open;
/// also the byte offset right after it.
pub fn var_insert(text: &str, caret: usize, name: &str) -> (String, usize) {
    let caret = snap(text, caret);
    let start = open_var(text, caret).map_or(caret, |(s, _)| s);
    let ins = format!("{{{{{name}}}}}");
    (format!("{}{ins}{}", &text[..start], &text[caret..]), start + ins.len())
}

/// Where the last edit ended, for fields that do not expose their cursor: `new` differs from `old` by one typed,
/// pasted or deleted run, so the caret sits at the end of what is not common suffix.
pub fn caret_after_edit(old: &str, new: &str) -> usize {
    let prefix = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix.min(old.len().min(new.len()));
    let suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    snap(new, new.len() - suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_indents_and_splits_matching_delimiters() {
        let c = enter("    {}", 5, 5, 3);
        assert_eq!(c.text, "    {\n        \n    }");
        assert_eq!((c.anchor, c.caret), (14, 14));
        assert_eq!(enter("[ ]", 1, 1, 3).text, "[\n    \n]");
        assert_eq!(enter("  <a></a>", 5, 5, 4).text, "  <a>\n      \n  </a>");
        assert_eq!(enter("<a/>", 4, 4, 4).text, "<a/>\n");
        for source in ["hello>", "<!-- <a>", "<![CDATA[<a>", "<a x='>"] {
            assert_eq!(enter(source, source.len(), source.len(), 4).text, format!("{source}\n"));
        }
        assert_eq!(enter("<a x='>'>", 9, 9, 4).text, "<a x='>'>\n    ");
        assert_eq!(enter("<a></b>", 3, 3, 4).text, "<a>\n    </b>");
        assert_eq!(enter("  abc", 5, 5, 3).text, "  abc\n  ");
        assert_eq!(enter("{\n    ", 6, 6, 3).text, "{\n    \n    ");
        assert_eq!(enter("<a>\n    ", 8, 8, 4).text, "<a>\n    \n    ");
        assert_eq!(enter("  abc", 5, 2, 3).text, "  \n  ");
        assert_eq!(enter("\"{", 2, 2, 3).text, "\"{\n");
        assert_eq!(enter("// {", 4, 4, 3).text, "// {\n");
        assert_eq!(
            enter("{\r\n    []\r\n}", 8, 8, 3).text,
            "{\r\n    [\r\n        \r\n    ]\r\n}"
        );
    }

    #[test]
    fn line_numbers_include_empty_and_trailing_lines() {
        assert_eq!(line_numbers(""), "1");
        assert_eq!(line_numbers("a\n\n"), "1\n2\n3");
    }

    fn names() -> Vec<String> {
        ["host", "db.port", "token"].map(String::from).to_vec()
    }

    #[test]
    fn braces_open_the_list_and_filter_it() {
        assert_eq!(var_matches(&names(), "{{", 2, false), names());
        assert_eq!(var_matches(&names(), "http://{{ho", 11, false), ["host"]);
        assert_eq!(var_matches(&names(), "{{port", 6, false), ["db.port"]);
        assert!(
            var_matches(&names(), "{{host}}", 8, false).is_empty(),
            "a closed one is done"
        );
        assert!(var_matches(&names(), "abc", 3, false).is_empty());
        assert_eq!(var_matches(&names(), "abc", 3, true), names(), "Ctrl+Space lists all");
    }

    #[test]
    fn insert_completes_or_adds_at_the_caret() {
        assert_eq!(var_insert("http://{{ho", 11, "host"), ("http://{{host}}".into(), 15));
        assert_eq!(var_insert("a/b", 2, "x"), ("a/{{x}}b".into(), 7));
        assert_eq!(
            var_insert("é", 99, "x"),
            ("é{{x}}".into(), 7),
            "an out-of-range caret is clamped"
        );
    }

    #[test]
    fn caret_follows_the_last_edit() {
        assert_eq!(caret_after_edit("ab", "a{{b"), 3);
        assert_eq!(caret_after_edit("", "{{"), 2);
        assert_eq!(caret_after_edit("abcd", "ad"), 1);
        assert_eq!(caret_after_edit("aa", "aaa"), 3);
        assert_eq!(caret_after_edit("same", "same"), 4);
        // Never inside a character: "é" and "è" share their first byte.
        assert!(
            ["éx", "èx"]
                .iter()
                .all(|n| n.is_char_boundary(caret_after_edit("éx", n)))
        );
    }

    #[test]
    fn tab_preserves_selected_lines_and_direction() {
        let change = indent("ab\ncd\nef", 4, 1, false);
        assert_eq!(change.text, "    ab\n    cd\nef");
        assert_eq!((change.anchor, change.caret), (12, 5));
        assert_eq!(indent("ab\ncd", 0, 3, false).text, "    ab\ncd");
        assert_eq!(indent("hello world", 0, 5, false).text, "    hello world");
        assert_eq!(indent("ab", 1, 1, false).text, "a    b");
        assert_eq!(indent("", 0, 0, false).text, "    ");
    }

    #[test]
    fn offsets_inside_a_character_or_past_the_end_are_safe() {
        // "é" is two bytes: offset 1 is inside it and snaps back to its start.
        assert_eq!(indent("é", 1, 1, false).text, "    é");
        assert_eq!(indent("ab", 99, 99, false).text, "ab    ");
        let change = indent("é\r\n猫", 0, 7, false);
        assert_eq!(change.text, "    é\r\n    猫");
        assert_eq!((change.anchor, change.caret), (4, 15));
    }

    #[test]
    fn unindent_handles_mixed_whitespace_and_carets_in_the_indent() {
        let text = "    a\r\n\tb\r\n  c\r\nd";
        let change = indent(text, text.len(), 2, true);
        assert_eq!(change.text, "a\r\nb\r\nc\r\nd");
        assert_eq!((change.anchor, change.caret), (10, 0));
        let change = indent("  é", 1, 1, true);
        assert_eq!(change.text, "é");
        assert_eq!((change.anchor, change.caret), (0, 0));
        assert_eq!(indent("a\n", 2, 2, true).text, "a\n");
    }
}
