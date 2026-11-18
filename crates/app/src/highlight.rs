//! Bounded, display-only syntax colors. The native input remains the source of truth.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use fastapiclient_core::http::{
    AUTO_TEXT_LIMIT, FormatError, MAX_BODY, pretty_json_limited, pretty_xml_limited, small_text,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color, StyleModifier, Theme, ThemeItem, ThemeSettings},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

use crate::{CodeEditing, ColorLine, ColorRun, ColoredText, Main};

pub fn response_kind(text: &str, content_type: &str) -> i32 {
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if matches!(mime.as_str(), "text/html" | "application/xhtml+xml") {
        return 0;
    }
    let lower = text
        .trim_start()
        .chars()
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return 0;
    }
    if mime == "application/json" || mime.ends_with("+json") {
        return 3;
    }
    if matches!(mime.as_str(), "application/xml" | "text/xml") || mime.ends_with("+xml") {
        return 4;
    }
    let text = text.trim_start();
    if text.starts_with(['{', '[']) {
        return 3;
    }
    if text.starts_with('<') {
        return 4;
    }
    0
}

#[derive(Debug, PartialEq)]
struct Run {
    text: String,
    kind: i32,
}

fn append(lines: &mut Vec<Vec<Run>>, text: &str, kind: i32) {
    for part in text.split_inclusive('\n') {
        let token = part.strip_suffix('\n').unwrap_or(part);
        let runs = lines.last_mut().unwrap();
        if !token.is_empty() {
            if let Some(last) = runs.last_mut().filter(|run| run.kind == kind) {
                last.text.push_str(token);
            } else {
                runs.push(Run {
                    text: token.into(),
                    kind,
                });
            }
        }
        if part.ends_with('\n') {
            lines.push(vec![]);
        }
    }
}

// Keep the old token colors aligned with an edit until the debounced parser supplies exact colors.
fn edited_colors(old: &str, new: &str, lines: &[Vec<Run>]) -> Vec<Vec<Run>> {
    let mut start = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = old[start..]
        .bytes()
        .rev()
        .zip(new[start..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let end = old.len() - suffix;
    let mut result = vec![vec![]];
    let mut offset = 0;
    let mut inserted = false;
    for (index, line) in lines.iter().enumerate() {
        // Newlines take part in the edit range, including when the last line is empty.
        for run in line
            .iter()
            .map(|run| (run.text.as_str(), run.kind))
            .chain((index + 1 < lines.len()).then_some(("\n", 0)))
        {
            let (text, kind) = run;
            let next = offset + text.len();
            if offset < start {
                append(&mut result, &text[..text.len().min(start - offset)], kind);
            }
            if !inserted && next >= start {
                append(&mut result, &new[start..new.len() - suffix], kind);
                inserted = true;
            }
            if next > end {
                append(&mut result, &text[end.saturating_sub(offset)..], kind);
            }
            offset = next;
        }
    }
    if !inserted {
        append(&mut result, &new[start..new.len() - suffix], 0);
    }
    result
}

fn colored_text(text: String, kind: i32, lines: Vec<Vec<Run>>) -> ColoredText {
    let lines = lines
        .into_iter()
        .map(|runs| ColorLine {
            runs: ModelRc::new(VecModel::from(
                runs.into_iter()
                    .map(|run| ColorRun {
                        text: run.text.into(),
                        kind: run.kind,
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect::<Vec<_>>();
    ColoredText {
        source: text.into(),
        kind,
        lines: ModelRc::new(VecModel::from(lines)),
    }
}

// Colors encode semantic roles; Slint supplies the actual light/dark palette without reparsing.
fn theme() -> Theme {
    Theme {
        settings: ThemeSettings {
            foreground: Some(Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }),
            ..Default::default()
        },
        scopes: [
            ("string", 1),
            ("constant.numeric", 2),
            ("entity.name.tag", 3),
            ("comment", 4),
            ("invalid", 5),
            // Match the key's string scope as well, so generic strings cannot override it.
            ("meta.structure.dictionary.key.json string", 6),
            ("constant.language", 7),
            ("entity.other.attribute-name", 8),
            ("string.quoted.double.xml, string.quoted.single.xml", 9),
        ]
        .into_iter()
        .map(|(scope, kind)| ThemeItem {
            scope: scope.parse().expect("static syntax scope"),
            style: StyleModifier {
                foreground: Some(Color {
                    r: kind,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
                ..Default::default()
            },
        })
        .collect(),
        ..Default::default()
    }
}

fn colors(text: &str, kind: i32, limit: usize, syntaxes: &SyntaxSet, theme: &Theme) -> Option<Vec<Vec<Run>>> {
    if text.len() > limit {
        return None;
    }
    let syntax = syntaxes.find_syntax_by_extension(match kind {
        3 => "json",
        4 => "xml",
        _ => return None,
    })?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = vec![];
    for line in LinesWithEndings::from(text) {
        let mut runs: Vec<Run> = vec![];
        for (style, token) in highlighter.highlight_line(line, syntaxes).ok()? {
            let token = token.trim_end_matches('\n');
            if token.is_empty() {
                continue;
            }
            let kind = i32::from(style.foreground.r);
            if let Some(last) = runs.last_mut().filter(|last| last.kind == kind) {
                last.text.push_str(token);
            } else {
                runs.push(Run {
                    text: token.into(),
                    kind,
                });
            }
        }
        lines.push(runs);
    }
    if text.is_empty() || text.ends_with('\n') {
        lines.push(vec![]);
    }
    Some(lines)
}

struct Job {
    text: String,
    kind: i32,
    slot: usize,
    revision: u64,
    automatic: bool,
}

const MANUAL_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

// Notice: 0 success, 1 invalid document, 2 output limit. Automatic overflow stays silently raw.
fn response_display(text: &str, kind: i32, automatic: bool) -> (String, i32) {
    if automatic && !small_text(text) {
        return (String::new(), 2);
    }
    let limit = if automatic {
        AUTO_TEXT_LIMIT
    } else {
        MANUAL_OUTPUT_LIMIT
    };
    // Lossy UTF-8 decoding can expand each captured byte to a three-byte replacement character.
    let formatted = match kind {
        3 => pretty_json_limited(text, MAX_BODY * 3, limit),
        4 => match roxmltree::Document::parse(text) {
            Ok(doc) if crate::payload::reindent_is_safe(&doc, text) => pretty_xml_limited(text, MAX_BODY * 3, limit),
            _ => Err(FormatError::Invalid),
        },
        _ => Err(FormatError::Invalid),
    };
    match formatted {
        Ok(text) => (text, 0),
        Err(FormatError::Invalid) => (text.into(), 1),
        Err(FormatError::Limit) => (String::new(), 2),
    }
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod response_tests;

pub fn response(ui: &Main, text: &str, content_type: &str) {
    ui.set_response_ready(false);
    ui.set_response_pretty("".into());
    ui.set_response_notice(0);
    ui.global::<CodeEditing>().set_response(ColoredText::default());
    ui.set_response_kind(response_kind(text, content_type));
    ui.set_response_body(text.into());
    ui.invoke_response_view(true);
}

pub fn clear_response(ui: &Main) {
    ui.set_has_response(false);
    ui.invoke_response_view(true);
    ui.set_response_ready(false);
}

pub fn wire(ui: &Main) {
    let pending = Arc::new(Mutex::new([None::<Job>, None]));
    let revisions = Arc::new([AtomicU64::new(0), AtomicU64::new(0)]);
    let (wake, jobs) = mpsc::sync_channel(1);
    let weak = ui.as_weak();
    std::thread::spawn({
        let (pending, revisions) = (pending.clone(), revisions.clone());
        move || {
            // Loaded only after the first eligible document, never on an empty startup.
            let mut syntaxes = None;
            let theme = theme();
            while jobs.recv().is_ok() {
                for slot in 0..2 {
                    let job = pending.lock().unwrap()[slot].take();
                    let Some(job) = job else { continue };
                    if revisions[slot].load(Ordering::Relaxed) != job.revision {
                        continue;
                    }
                    let (display, notice) = if slot == 1 {
                        response_display(&job.text, job.kind, job.automatic)
                    } else {
                        (job.text, 0)
                    };
                    let limit = if slot == 0 {
                        AUTO_TEXT_LIMIT
                    } else {
                        MANUAL_OUTPUT_LIMIT
                    };
                    let lines = if notice == 2 {
                        vec![]
                    } else {
                        let syntaxes = syntaxes.get_or_insert_with(SyntaxSet::load_defaults_newlines);
                        colors(&display, job.kind, limit, syntaxes, &theme).unwrap_or_default()
                    };
                    let revisions = revisions.clone();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if revisions[slot].load(Ordering::Relaxed) != job.revision {
                            return;
                        }
                        let color_kind = if lines.is_empty() { 0 } else { job.kind };
                        let result = colored_text(display.clone(), color_kind, lines);
                        let editing = ui.global::<CodeEditing>();
                        if job.slot == 0 {
                            editing.set_request(result);
                        } else {
                            editing.set_response(result);
                            ui.set_response_processing(false);
                            ui.set_response_pretty(display.into());
                            ui.set_response_notice(if job.automatic && notice == 2 { 0 } else { notice });
                            ui.set_response_ready(!job.automatic || notice != 2);
                            ui.set_response_mode(i32::from(notice != 2));
                        }
                    });
                }
            }
        }
    });
    let weak_response = ui.as_weak();
    let (response_pending, response_revisions, response_wake) = (pending.clone(), revisions.clone(), wake.clone());
    ui.on_response_view(move |automatic| {
        let Some(ui) = weak_response.upgrade() else { return };
        let revision = response_revisions[1].fetch_add(1, Ordering::Relaxed) + 1;
        response_pending.lock().unwrap()[1] = None;
        ui.set_response_processing(false);
        if automatic {
            ui.set_response_mode(0);
        }
        let text = ui.get_response_body();
        if !ui.get_has_response()
            || ui.get_response_kind() == 0
            || (automatic && (!ui.get_auto_beautify() || !small_text(&text)))
            || (!automatic && ui.get_response_mode() == 0)
        {
            ui.set_response_mode(0);
            return;
        }
        if ui.get_response_ready() {
            let fits = !automatic || small_text(&ui.get_response_pretty());
            ui.set_response_mode(i32::from(fits && ui.get_response_notice() != 2));
            return;
        }
        ui.set_response_mode(1);
        ui.set_response_processing(true);
        response_pending.lock().unwrap()[1] = Some(Job {
            text: text.to_string(),
            kind: ui.get_response_kind(),
            slot: 1,
            revision,
            automatic,
        });
        let _ = response_wake.try_send(());
    });
    let timers = [slint::Timer::default(), slint::Timer::default()];
    let weak = ui.as_weak();
    ui.global::<CodeEditing>().on_highlight(move |text, kind, slot| {
        let Ok(slot @ 0) = usize::try_from(slot) else {
            return;
        };
        let revision = revisions[slot].fetch_add(1, Ordering::Relaxed) + 1;
        timers[slot].stop();
        pending.lock().unwrap()[slot] = None;
        if !matches!(kind, 3 | 4) || text.is_empty() || !small_text(&text) {
            return;
        }
        if slot == 0
            && let Some(ui) = weak.upgrade()
        {
            use slint::Model;
            let editing = ui.global::<CodeEditing>();
            let previous = editing.get_request();
            if previous.kind == kind && !previous.source.is_empty() && previous.source != text {
                let lines = previous
                    .lines
                    .iter()
                    .map(|line| {
                        line.runs
                            .iter()
                            .map(|run| Run {
                                text: run.text.to_string(),
                                kind: run.kind,
                            })
                            .collect()
                    })
                    .collect::<Vec<_>>();
                let lines = edited_colors(&previous.source, &text, &lines);
                editing.set_request(colored_text(text.to_string(), kind, lines));
            }
        }
        let (pending, wake) = (pending.clone(), wake.clone());
        timers[slot].start(slint::TimerMode::SingleShot, Duration::from_millis(100), move || {
            pending.lock().unwrap()[slot] = Some(Job {
                text: text.to_string(),
                kind,
                slot,
                revision,
                automatic: true,
            });
            let _ = wake.try_send(());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastapiclient_core::http::AUTO_TEXT_LIMIT;

    #[test]
    fn detects_only_json_and_xml_without_an_automatic_limit() {
        for (body, mime, expected) in [
            ("{}", "application/problem+json; charset=utf-8", 3),
            ("<a/>", "application/soap+xml", 4),
            ("[1]", "", 3),
            ("<a/>", "", 4),
            ("<!DOCTYPE html><html/>", "", 0),
            ("<html/>", "", 0),
            ("<p/>", "text/html", 0),
            ("<html/>", "application/xhtml+xml", 0),
            ("hello", "text/plain", 0),
        ] {
            assert_eq!(response_kind(body, mime), expected, "{body}: {mime}");
        }
        assert_eq!(response_kind(&"x".repeat(AUTO_TEXT_LIMIT + 1), "application/json"), 3);
    }

    #[test]
    fn syntax_roles_distinguish_json_keys_and_xml_attributes() {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let theme = theme();
        let check = |source: &str, syntax, expected: &[(&str, i32)]| {
            let lines = colors(source, syntax, AUTO_TEXT_LIMIT, &syntaxes, &theme).unwrap();
            let roles: Vec<_> = lines
                .iter()
                .enumerate()
                .flat_map(|(index, runs)| {
                    (index > 0).then_some(0).into_iter().chain(
                        runs.iter()
                            .flat_map(|run| std::iter::repeat_n(run.kind, run.text.len())),
                    )
                })
                .collect();
            assert_eq!(roles.len(), source.len());
            for &(text, role) in expected {
                let start = source.find(text).unwrap();
                assert!(
                    roles[start..start + text.len()].iter().all(|&kind| kind == role),
                    "{text:?} expected role {role}, got {:?}; runs: {lines:?}",
                    &roles[start..start + text.len()]
                );
            }
        };
        check(
            r#"{"outer": {"猫\"key": "value: \"café\"", "number": 12.5, "flags": [true, false, null]}}"#,
            3,
            &[
                ("outer", 6),
                (r#"猫\"key"#, 6),
                (r#"value: \"café\""#, 1),
                ("12.5", 2),
                ("true", 7),
                ("false", 7),
                ("null", 7),
                (": {", 0),
                (", ", 0),
            ],
        );
        check(
            r#"{"unfinished": "still: typing"#,
            3,
            &[("unfinished", 6), ("still: typing", 1)],
        );
        check(
            "<root name=\"猫\" other='café'><!-- comment --><child/></root>",
            4,
            &[
                ("root", 3),
                ("name", 8),
                ("other", 8),
                ("猫", 9),
                ("café", 9),
                ("<!-- comment -->", 4),
                ("child", 3),
            ],
        );
        check(r#"{"bad": "\q"}"#, 3, &[(r#"\q"#, 5)]);
    }

    #[test]
    fn colors_preserve_source_and_multiline_state() {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let theme = theme();
        for (source, kind) in [
            ("{\n\t\"猫\": [true, 1, \"{{host}}\"],\n\"unfinished\": \"", 3),
            ("<r>\n<!-- first\nsecond -->\n<a x=\"猫\"/>\n</r>\n", 4),
            ("", 3),
        ] {
            let lines = colors(source, kind, AUTO_TEXT_LIMIT, &syntaxes, &theme).unwrap();
            let rebuilt = lines
                .iter()
                .map(|runs| runs.iter().map(|run| run.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(rebuilt, source);
            if !source.is_empty() {
                assert!(lines.iter().flatten().any(|run| run.kind != 0));
            }
        }
        for length in [AUTO_TEXT_LIMIT - 1, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT + 1] {
            assert_eq!(
                colors(&"x".repeat(length), 3, AUTO_TEXT_LIMIT, &syntaxes, &theme).is_some(),
                length <= AUTO_TEXT_LIMIT
            );
        }
    }

    #[test]
    fn pending_edits_preserve_current_text_and_unaffected_colors() {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let theme = theme();
        let old = "{\n  \"name\": \"é猫\",\n  \"n\": 123\n}\n";
        for new in [
            old.replace("é猫", "è猫!"),
            old.replace("é猫", "e\u{301}猫"),
            old.replace("é猫", ""),
            old.replace("\"name\": \"é猫\",\n", ""),
            old.replace("123", "[\n1,\n2\n]"),
            format!("\n{old}"),
            format!("{old}more"),
            "<completely different/>".into(),
            String::new(),
        ] {
            let lines = colors(old, 3, AUTO_TEXT_LIMIT, &syntaxes, &theme).unwrap();
            let lines = edited_colors(old, &new, &lines);
            let rebuilt = lines
                .iter()
                .map(|runs| runs.iter().map(|run| run.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(rebuilt, new);
            if new.contains("123") {
                assert!(lines.iter().flatten().any(|run| run.kind == 2 && run.text == "123"));
            }
        }
        let mut source = "\"é\"".to_string();
        let mut lines = colors(&source, 3, AUTO_TEXT_LIMIT, &syntaxes, &theme).unwrap();
        for new in ["\"è\"", "\"è猫\"", "\"è猫!\"", "\"è\"", "", "\nnew\n"] {
            lines = edited_colors(&source, new, &lines);
            assert_eq!(
                lines
                    .iter()
                    .map(|runs| runs.iter().map(|run| run.text.as_str()).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
                new
            );
            if new.starts_with('"') {
                assert!(lines.iter().flatten().all(|run| run.kind == 1));
            }
            source = new.into();
        }
    }
}
