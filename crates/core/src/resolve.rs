//! Inheritance (Project < Folder < Request) and `{{variable}}` substitution.
use std::fmt::Write;

use chrono::{DateTime, FixedOffset, Local, Months, SecondsFormat, TimeDelta};
use serde_json::Value;

use crate::model::{Auth, KeyValue};

/// Merges header lists from the outermost parent to the request. A later entry replaces an earlier one with the same
/// name (case-insensitive). A disabled entry is ignored, as in Insomnia: it neither sends nor cancels an inherited
/// header (imports are full of disabled rows that must not hide the project's).
pub fn merge_headers(chain: &[&[KeyValue]]) -> Vec<KeyValue> {
    let mut out: Vec<KeyValue> = vec![];
    for kv in chain
        .iter()
        .flat_map(|list| list.iter())
        .filter(|kv| kv.enabled && !kv.key.is_empty())
    {
        out.retain(|e| !e.key.eq_ignore_ascii_case(&kv.key));
        out.push(kv.clone());
    }
    out
}

/// The nearest non-`Inherit` auth, scanning from the request outwards; `None` when nobody sets one.
pub fn effective_auth<'a>(chain: &[&'a Auth]) -> &'a Auth {
    static NONE: Auth = Auth::None;
    chain
        .iter()
        .rev()
        .find(|a| !matches!(a, Auth::Inherit))
        .copied()
        .unwrap_or(&NONE)
}

pub(crate) fn lookup(env: &Value, path: &str) -> Option<String> {
    let found = path
        .split('.')
        .try_fold(env, |v, key| v.get(key).or_else(|| v.get(key.parse::<usize>().ok()?)))?;
    Some(match found {
        Value::String(s) => s.clone(),
        Value::Null => return None,
        other => other.to_string(),
    })
}

/// Every name `{{...}}` can resolve in `env`, nested objects as `a.b`, for autocomplete. Arrays and null are skipped:
/// `lookup` cannot reach into the first and treats the second as undefined.
pub fn names(env: &Value) -> Vec<String> {
    fn walk(v: &Value, prefix: &str, out: &mut Vec<String>) {
        let Value::Object(map) = v else { return };
        for (k, v) in map {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                Value::Object(_) => walk(v, &path, out),
                Value::Null | Value::Array(_) => {}
                _ => out.push(path),
            }
        }
    }
    let mut out = vec![];
    walk(env, "", &mut out);
    out
}

/// Ready-made function expressions offered by autocomplete after the environment's variables.
pub const FUNCTIONS: [&str; 5] = [
    "$date",
    "$date | unix",
    "$date | format('%Y-%m-%d')",
    "$date | add('-1 day')",
    "$date | add('-1 month') | format('%Y-%m-%d')",
];

/// Arguments of a call like `add('-1 day')` or `$response('A', '$.x', 3600)`: quoted strings (a comma inside quotes
/// stays) or bare values, trimmed. `None` when `call` is not `name(...)`.
pub(crate) fn args(call: &str, name: &str) -> Option<Vec<String>> {
    let inner = call.strip_prefix(name)?.trim().strip_prefix('(')?.strip_suffix(')')?;
    let (mut out, mut cur, mut quote) = (vec![], String::new(), None);
    for c in inner.chars() {
        match (c, quote) {
            ('\'' | '"', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            (',', None) => out.push(std::mem::take(&mut cur)),
            (c, Some(_)) => cur.push(c),
            (c, None) if !c.is_whitespace() => cur.push(c),
            _ => {}
        }
    }
    (quote.is_none()).then(|| {
        out.push(cur);
        out
    })
}

/// The one argument of `name(...)`.
fn arg(filter: &str, name: &str) -> Option<String> {
    let mut a = args(filter, name)?;
    (a.len() == 1).then(|| a.remove(0))
}

/// `$.a.b` or `$.list[0].id` in a JSON value, as text. Shortcut: no wildcards or filters; a JSONPath crate if needed.
pub(crate) fn json_path(v: &Value, path: &str) -> Option<String> {
    let p = path.trim().strip_prefix('$')?.replace('[', ".").replace(']', "");
    let p = p.trim_start_matches('.');
    if p.is_empty() {
        return Some(v.to_string());
    }
    lookup(v, p)
}

fn shift(t: DateTime<FixedOffset>, spec: &str) -> Option<DateTime<FixedOffset>> {
    let (n, unit) = spec.trim().split_once(char::is_whitespace)?;
    let n: i64 = n.trim().trim_start_matches('+').parse().ok()?;
    let unit = unit.trim().trim_end_matches('s');
    let months = |m: i64| {
        let by = Months::new(u32::try_from(m.unsigned_abs()).ok()?);
        if m < 0 {
            t.checked_sub_months(by)
        } else {
            t.checked_add_months(by)
        }
    };
    match unit {
        "second" => t.checked_add_signed(TimeDelta::try_seconds(n)?),
        "minute" => t.checked_add_signed(TimeDelta::try_minutes(n)?),
        "hour" => t.checked_add_signed(TimeDelta::try_hours(n)?),
        "day" => t.checked_add_signed(TimeDelta::try_days(n)?),
        "week" => t.checked_add_signed(TimeDelta::try_weeks(n)?),
        "month" => months(n),
        "year" => months(n.checked_mul(12)?),
        _ => None,
    }
}

/// Evaluates `$date | add('-60 days') | format('%Y-%m-%d')`. `add` may repeat; `unix` or `format` ends the chain,
/// and without one the result is RFC 3339 (ISO-8601). `None` for anything it does not understand.
/// Shortcut: a `|` inside a format mask is not supported; upgrade with a real tokenizer if anyone needs it.
pub(crate) fn date_expr(expr: &str, now: DateTime<FixedOffset>) -> Option<String> {
    let mut parts = expr.split('|').map(str::trim);
    if parts.next()? != "$date" {
        return None;
    }
    let (mut t, mut out) = (now, None);
    for f in parts {
        if out.is_some() {
            return None;
        }
        if let Some(spec) = arg(f, "add") {
            t = shift(t, &spec)?;
        } else if f == "unix" {
            out = Some(t.timestamp().to_string());
        } else {
            let mask = arg(f, "format")?;
            let mut s = String::new();
            // `to_string()` would panic on a bad mask; `write!` reports it.
            write!(s, "{}", t.format(&mask)).ok()?;
            out = Some(s);
        }
    }
    Some(out.unwrap_or_else(|| t.to_rfc3339_opts(SecondsFormat::Secs, false)))
}

/// Replaces `{{name}}` (or `{{a.b}}`) with the value from `env`. Unknown names are left as written and returned, so
/// the UI can warn instead of sending a request with a silently wrong URL.
pub fn substitute(text: &str, env: &Value) -> (String, Vec<String>) {
    let now = Local::now().fixed_offset();
    substitute_with(text, env, &mut |name| date_expr(name, now))
}

/// `substitute` with `$function` names (`$date`, `$response(...)`) evaluated by `func`.
pub fn substitute_with(text: &str, env: &Value, func: &mut dyn FnMut(&str) -> Option<String>) -> (String, Vec<String>) {
    let (mut out, mut missing) = (String::with_capacity(text.len()), vec![]);
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else { break };
        let name = rest[start + 2..start + 2 + len].trim();
        out.push_str(&rest[..start]);
        let value = if name.starts_with('$') {
            func(name)
        } else {
            lookup(env, name)
        };
        match value {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str(&rest[start..start + 2 + len + 2]);
                if !missing.iter().any(|m| m == name) {
                    missing.push(name.to_string());
                }
            }
        }
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    (out, missing)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn kv(k: &str, v: &str, enabled: bool) -> KeyValue {
        KeyValue {
            enabled,
            ..KeyValue::new(k, v)
        }
    }

    #[test]
    fn request_header_wins_case_insensitively() {
        let project = [kv("Accept", "a", true), kv("X-Id", "p", true)];
        let folder = [kv("x-id", "f", true)];
        let request = [kv("ACCEPT", "r", true)];
        let merged = merge_headers(&[&project, &folder, &request]);
        let pairs: Vec<_> = merged.iter().map(|h| (h.key.as_str(), h.value.as_str())).collect();
        assert_eq!(pairs, [("x-id", "f"), ("ACCEPT", "r")]);
    }

    #[test]
    fn disabled_request_header_does_not_hide_the_inherited_one() {
        let project = [kv("X-Id", "p", true)];
        let request = [kv("X-Id", "r", false)];
        let merged = merge_headers(&[&project, &request]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, "p");
    }

    #[test]
    fn nearest_non_inherit_auth_wins() {
        let (project, folder) = (Auth::Bearer("p".into()), Auth::Inherit);
        let request = Auth::Inherit;
        assert_eq!(effective_auth(&[&project, &folder, &request]), &project);
        assert_eq!(effective_auth(&[&Auth::Inherit]), &Auth::None);
        let off = Auth::None;
        assert_eq!(effective_auth(&[&project, &off]), &Auth::None);
    }

    #[test]
    fn substitutes_flat_nested_and_non_string_values() {
        let env = json!({"host": "api.test", "db": {"port": 5432}, "debug": true});
        let (out, missing) = substitute("https://{{host}}:{{ db.port }}/?d={{debug}}", &env);
        assert_eq!(out, "https://api.test:5432/?d=true");
        assert!(missing.is_empty());
    }

    #[test]
    fn names_lists_leaves_with_dotted_paths() {
        let env = json!({"host": "h", "db": {"port": 1, "user": {"name": "x"}}, "n": null, "l": [1]});
        let mut got = names(&env);
        got.sort();
        assert_eq!(got, ["db.port", "db.user.name", "host"]);
        assert!(names(&Value::Null).is_empty());
    }

    #[test]
    fn unknown_and_unterminated_are_left_as_written() {
        let (out, missing) = substitute("{{a}} {{a}} {{b.c}} {{open", &json!({}));
        assert_eq!(out, "{{a}} {{a}} {{b.c}} {{open");
        assert_eq!(missing, ["a", "b.c"]);
    }

    fn at(text: &str) -> (String, Vec<String>) {
        let now = DateTime::parse_from_rfc3339("2026-03-31T10:30:00-03:00").unwrap();
        substitute_with(text, &json!({"date": "env"}), &mut |n| date_expr(n, now))
    }

    #[test]
    fn date_function_formats_and_shifts() {
        assert_eq!(at("{{ $date }}").0, "2026-03-31T10:30:00-03:00");
        assert_eq!(at("{{$date | unix}}").0, "1774963800");
        assert_eq!(at("{{ $date | add('-60 days') }}").0, "2026-01-30T10:30:00-03:00");
        assert_eq!(
            at("{{ $date | add('-1 month') | format('%Y-%m-%d') }}").0,
            "2026-02-28",
            "end of month clamps"
        );
        assert_eq!(at("{{ $date | add('+2 hours') | format(\"%H:%M\") }}").0, "12:30");
        assert_eq!(at("{{date}}").0, "env", "$ keeps functions apart from variables");
    }

    #[test]
    fn bad_date_expressions_are_missing() {
        for bad in [
            "$date | nope",
            "$date | add('1 fortnight')",
            "$date | add('x days')",
            "$date | format('%Q')",
            "$date | unix | format('%Y')",
            "$now",
        ] {
            let text = format!("{{{{ {bad} }}}}");
            let (out, missing) = at(&text);
            assert_eq!((out, missing), (text, vec![bad.to_string()]));
        }
    }

    #[test]
    fn json_path_reaches_keys_and_indexes() {
        let v = json!({"a": {"b": "x"}, "l": [{"id": 1}, {"id": 2}], "n": null});
        assert_eq!(json_path(&v, "$.a.b").as_deref(), Some("x"));
        assert_eq!(json_path(&v, "$.l[1].id").as_deref(), Some("2"));
        assert_eq!(json_path(&v, "$.l.0.id").as_deref(), Some("1"));
        assert_eq!(json_path(&v, "$.a").as_deref(), Some(r#"{"b":"x"}"#));
        assert_eq!(json_path(&v, "$.nope"), None);
        assert_eq!(json_path(&v, "$.n"), None);
        assert_eq!(json_path(&v, "a.b"), None, "must start with $");
    }

    #[test]
    fn args_split_on_commas_outside_quotes() {
        assert_eq!(
            args("$response('A, B', \"$.x\", 3600)", "$response").unwrap(),
            ["A, B", "$.x", "3600"]
        );
        assert_eq!(args("add('-1 day')", "add").unwrap(), ["-1 day"]);
        assert_eq!(args("add('x", "add"), None);
        assert_eq!(args("unix", "add"), None);
    }

    #[test]
    fn no_environment_means_everything_is_missing() {
        assert_eq!(substitute("{{x}}", &Value::Null).1, ["x"]);
    }
}
