//! Import of an Insomnia v5 export (`collection.insomnia.rest/5.x` or `spec.insomnia.rest/5.x`, YAML). The file is
//! read as loose data (`serde_json::Value`) on purpose: Insomnia's schema has a dozen auth kinds and several request
//! kinds, and only a handful of fields map onto this app, so typed structs would mostly be ignored fields.
use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::exchange::ImportError;
use crate::model::{Auth, Basic, Body, Environment, Folder, Grant, KeyValue, OAuth2, Project, Request};

/// A value as text; null is empty, a number or bool is written out.
fn text(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The text of a field; a missing one is empty.
fn s(v: &Value, key: &str) -> String {
    v.get(key).map_or_else(String::new, text)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Insomnia's display order. Missing keys sort last, and the sort is stable, so those keep their file order.
fn sorted(items: &[Value]) -> Vec<&Value> {
    let key = |v: &Value| v.pointer("/meta/sortKey").and_then(Value::as_f64).unwrap_or(f64::MAX);
    let mut out: Vec<&Value> = items.iter().collect();
    out.sort_by(|a, b| key(a).total_cmp(&key(b)));
    out
}

/// `{{ _.name }}` becomes `{{name}}` (Insomnia's root-variable prefix). Anything else, including `{% tags %}`, stays.
fn tpl(text: &str) -> String {
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else { break };
        let inner = &rest[start + 2..start + 2 + len];
        out.push_str(&rest[..start]);
        match inner.trim().strip_prefix("_.") {
            Some(name) => out.push_str(&format!("{{{{{name}}}}}")),
            None => out.push_str(&rest[start..start + 2 + len + 2]),
        }
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    out
}

fn pairs(items: &[Value]) -> Vec<KeyValue> {
    items
        .iter()
        .map(|p| KeyValue {
            key: tpl(&s(p, "name")),
            value: tpl(&s(p, "value")),
            description: s(p, "description"),
            enabled: !flag(p, "disabled"),
        })
        .collect()
}

fn body(b: &Value) -> Body {
    let mime = s(b, "mimeType");
    let text = tpl(&s(b, "text"));
    // shortcut: file fields of a multipart body are dropped (FormData has none); upgrade when the model gets them.
    let fields = || -> Vec<Value> {
        list(b, "params")
            .iter()
            .filter(|p| s(p, "type") != "file" && s(p, "fileName").is_empty())
            .cloned()
            .collect()
    };
    if mime.starts_with("multipart/form-data") {
        Body::FormData(pairs(&fields()))
    } else if mime == "application/x-www-form-urlencoded" {
        Body::FormUrl(pairs(&fields()))
    } else if !s(b, "fileName").is_empty() {
        // A file body is never imported: a shared collection must not be able to point Send at a local file
        // (an SSH key) and a foreign host. The user can pick the file again in the request.
        Body::None
    } else if text.is_empty() {
        Body::None
    } else if mime.contains("json") || mime == "application/graphql" {
        Body::Json(text)
    } else {
        // Sent byte for byte; `Json` would strip `//` comments from plain text.
        Body::Xml(text)
    }
}

fn auth(a: &Value) -> Auth {
    let kind = s(a, "type");
    if kind.is_empty() {
        return Auth::Inherit;
    }
    if flag(a, "disabled") {
        return Auth::None;
    }
    match kind.as_str() {
        // shortcut: a custom bearer `prefix` is dropped, the header is always `Bearer`.
        "bearer" => Auth::Bearer(tpl(&s(a, "token"))),
        "basic" => Auth::Basic(Basic {
            username: tpl(&s(a, "username")),
            password: tpl(&s(a, "password")),
        }),
        "oauth2" => {
            let grant = match s(a, "grantType").as_str() {
                "client_credentials" => Grant::ClientCredentials,
                "password" => Grant::Password,
                "authorization_code" => Grant::AuthorizationCode,
                _ => return Auth::None,
            };
            Auth::OAuth2(OAuth2 {
                grant,
                auth_url: tpl(&s(a, "authorizationUrl")),
                token_url: tpl(&s(a, "accessTokenUrl")),
                client_id: tpl(&s(a, "clientId")),
                client_secret: tpl(&s(a, "clientSecret")),
                scope: tpl(&s(a, "scope")),
                username: tpl(&s(a, "username")),
                password: tpl(&s(a, "password")),
            })
        }
        // An API key is a header or a query parameter here, not an Auth kind; see `api_key`.
        _ => Auth::None,
    }
}

/// An `apikey` auth as a header, or as a query parameter when the flag is true.
fn api_key(a: &Value) -> Option<(KeyValue, bool)> {
    if s(a, "type") != "apikey" || flag(a, "disabled") {
        return None;
    }
    let kv = KeyValue::new(&tpl(&s(a, "key")), &tpl(&s(a, "value")));
    Some((kv, s(a, "addTo") == "queryParams"))
}

fn request(item: &Value) -> Request {
    let a = item.get("authentication").unwrap_or(&Value::Null);
    let (mut headers, mut query) = (pairs(list(item, "headers")), pairs(list(item, "parameters")));
    match api_key(a) {
        Some((kv, true)) => query.push(kv),
        Some((kv, false)) => headers.push(kv),
        None => {}
    }
    Request {
        name: s(item, "name"),
        method: s(item, "method").to_ascii_uppercase(),
        url: tpl(&s(item, "url")),
        query,
        headers,
        auth: if api_key(a).is_some() { Auth::None } else { auth(a) },
        body: item.get("body").map_or(Body::None, body),
    }
}

fn folder(item: &Value, parent: Option<usize>) -> Folder {
    let a = item.get("authentication").unwrap_or(&Value::Null);
    let mut headers = pairs(list(item, "headers"));
    // shortcut: an API key in the query string has nowhere to go on a folder; only the header form is kept.
    if let Some((kv, false)) = api_key(a) {
        headers.push(kv);
    }
    Folder {
        name: s(item, "name"),
        parent,
        headers,
        auth: if api_key(a).is_some() { Auth::None } else { auth(a) },
        requests: vec![],
    }
}

/// Folders and requests of one level, in Insomnia's order. WebSocket, Socket.IO and gRPC items are skipped.
fn items(project: &mut Project, level: &[Value], parent: Option<usize>) {
    for item in sorted(level) {
        if item.get("children").is_some_and(Value::is_array) {
            project.folders.push(folder(item, parent));
            let index = project.folders.len() - 1;
            items(project, list(item, "children"), Some(index));
        } else if item.get("method").is_some()
            && item.get("reflectionApi").is_none()
            && !s(item.get("meta").unwrap_or(&Value::Null), "id").starts_with("greq_")
        {
            let requests = parent.map_or(&mut project.requests, |i| &mut project.folders[i].requests);
            requests.push(request(item));
        }
    }
}

/// Object values merge key by key, anything else is replaced, as Insomnia resolves a sub-environment over its base.
fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                merge(b.entry(k).or_insert(Value::Null), v);
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// The environment's own data plus its enabled key/value rows (the "kv" editor mode).
fn env_data(env: &Value) -> Value {
    let mut data = env.get("data").cloned().unwrap_or(Value::Null);
    if !data.is_object() {
        data = Value::Object(Map::new());
    }
    for row in list(env, "kvPairData") {
        if row.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
            data[s(row, "name")] = row.get("value").cloned().unwrap_or(Value::Null);
        }
    }
    data
}

/// Moves Insomnia's `GLOBAL_HEADERS` convention (headers a plugin sent on every request there) out of an
/// environment's values and into project headers, as written. The first definition of a header wins.
fn take_global_headers(values: &mut Value, headers: &mut Vec<KeyValue>) {
    let Some(Value::Object(globals)) = values.as_object_mut().and_then(|o| o.remove("GLOBAL_HEADERS")) else {
        return;
    };
    for (key, value) in globals {
        if !headers.iter().any(|h| h.key == key) {
            headers.push(KeyValue::new(&key, &tpl(&text(&value))));
        }
    }
}

/// Every sub-environment becomes one holding base + its own values, so each is complete on its own. With no
/// sub-environments the base itself is the only one. `GLOBAL_HEADERS` is not kept in them: see
/// `take_global_headers`.
fn environments(project: &mut Project, root: &Value) {
    let Some(env) = root.get("environments").filter(|e| e.is_object()) else {
        return;
    };
    let mut base = env_data(env);
    take_global_headers(&mut base, &mut project.headers);
    let subs = sorted(list(env, "subEnvironments"));
    let mut all: Vec<Environment> = subs
        .iter()
        .map(|sub| {
            let mut own = env_data(sub);
            take_global_headers(&mut own, &mut project.headers);
            let mut values = base.clone();
            merge(&mut values, &own);
            Environment {
                name: s(sub, "name"),
                values,
            }
        })
        .collect();
    if all.is_empty() && base.as_object().is_some_and(|o| !o.is_empty()) {
        let name = s(env, "name");
        all.push(Environment {
            name: if name.is_empty() { "Base".into() } else { name },
            values: base,
        });
    }
    project.active_env = all.first().map(|e| e.name.clone()).unwrap_or_default();
    project.environments = all;
}

/// Standard base64, no validation beyond the alphabet; `None` on anything else or non-UTF-8 output.
fn b64_decode(s: &str) -> Option<String> {
    let (mut out, mut bits, mut n) = (vec![], 0u32, 0);
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        (bits, n) = (bits << 6 | u32::from(v), n + 6);
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    String::from_utf8(out).ok()
}

/// One `{% response 'body', 'req_id', 'b64::<JSONPath>::46b', 'when-expired', max_age %}` as our
/// `$response('Request', '$.path', max_age)`. `None` (so the tag stays as written) for anything else: another
/// attribute, an XPath filter, a request that is not in the file.
fn response_tag(tag: &str, ids: &HashMap<String, String>) -> Option<String> {
    let a = crate::resolve::args(&format!("t({})", tag.trim().strip_prefix("response")?), "t")?;
    let (kind, id, filter, behavior) = (a.first()?, a.get(1)?, a.get(2)?, a.get(3)?);
    let name = ids.get(id).filter(|n| !n.contains('\''))?;
    let path = b64_decode(filter.strip_prefix("b64::")?.split("::").next()?)?;
    if kind != "body" || !path.starts_with('$') || path.contains('\'') {
        return None;
    }
    // Insomnia's max age is often `_.variable`; ours names the environment variable the same way, without `_.`.
    let expiry = match behavior.as_str() {
        "when-expired" => a.get(4).map(|e| e.strip_prefix("_.").unwrap_or(e)),
        "always" => Some("0"),
        _ => None,
    };
    Some(match expiry {
        Some(e) => format!("{{{{ $response('{name}', '{path}', '{e}') }}}}"),
        None => format!("{{{{ $response('{name}', '{path}') }}}}"),
    })
}

/// Rewrites every `{% response ... %}` in the strings of the file.
fn convert_tags(v: &mut Value, ids: &HashMap<String, String>) {
    match v {
        Value::String(t) if t.contains("{%") => {
            let (mut out, mut rest) = (String::new(), t.as_str());
            while let Some(start) = rest.find("{%") {
                let Some(len) = rest[start + 2..].find("%}") else { break };
                let end = start + 2 + len + 2;
                out.push_str(&rest[..start]);
                match response_tag(&rest[start + 2..end - 2], ids) {
                    Some(c) => out.push_str(&c),
                    None => out.push_str(&rest[start..end]),
                }
                rest = &rest[end..];
            }
            out.push_str(rest);
            *t = out;
        }
        Value::Array(items) => items.iter_mut().for_each(|i| convert_tags(i, ids)),
        Value::Object(map) => map.values_mut().for_each(|i| convert_tags(i, ids)),
        _ => {}
    }
}

/// `meta.id` → name of every item that has both, so a tag's `req_...` can become a request name.
fn request_ids(v: &Value, out: &mut HashMap<String, String>) {
    if let (Some(id), Some(name)) = (
        v.pointer("/meta/id").and_then(Value::as_str),
        v.get("name").and_then(Value::as_str),
    ) {
        out.insert(id.into(), name.into());
    }
    match v {
        Value::Array(items) => items.iter().for_each(|i| request_ids(i, out)),
        Value::Object(map) => map.values().for_each(|i| request_ids(i, out)),
        _ => {}
    }
}

/// Insomnia v5 exports are one project per file. The project gets a fresh id.
pub fn import(text: &str) -> Result<Project, ImportError> {
    let mut root: Value = serde_yaml_ng::from_str(text).map_err(|e| ImportError::Invalid(e.to_string()))?;
    let mut ids = HashMap::new();
    request_ids(&root, &mut ids);
    convert_tags(&mut root, &ids);
    let kind = s(&root, "type");
    if !(kind.starts_with("collection.insomnia.rest/5") || kind.starts_with("spec.insomnia.rest/5")) {
        return Err(ImportError::NotAProject);
    }
    let name = s(&root, "name");
    let mut project = Project::new(if name.is_empty() { "Insomnia" } else { &name });
    items(&mut project, list(&root, "collection"), None);
    environments(&mut project, &root);
    Ok(project)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_file_body_is_not_imported() {
        let b = json!({ "mimeType": "application/octet-stream", "fileName": "/home/u/.ssh/id_rsa" });
        assert_eq!(body(&b), Body::None);
    }

    const SAMPLE: &str = r#"
type: collection.insomnia.rest/5.0
schema_version: "5.1"
name: Shop
collection:
  - name: Root
    meta: { sortKey: 2 }
    headers:
      - { name: X-Team, value: "{{ _.team }}" }
    authentication: { type: bearer, token: "{{ _.token }}" }
    children:
      - name: Inner
        meta: { sortKey: 1 }
        children:
          - url: "{{ _.base }}/deep"
            name: Deep
            method: post
            meta: { id: req_1, sortKey: 1 }
            authentication: { type: none }
            body:
              mimeType: multipart/form-data
              params:
                - { name: a, value: "1" }
                - { name: f, type: file, fileName: /tmp/x }
                - { name: off, value: z, disabled: true }
      - url: "{{_.base}}/health"
        name: Health
        method: GET
        meta: { id: req_2, sortKey: 5 }
        parameters:
          - { name: q, value: "{% prompt 'a', 'b' %}" }
        authentication: { type: basic, username: u, password: p }
        body: { mimeType: application/json, text: '{"a": "{{ _.base }}"}' }
  - name: Socket
    url: ws://x
    meta: { id: ws-req_1 }
  - name: First
    url: http://x
    method: GET
    meta: { id: req_3, sortKey: 0 }
    authentication: { type: apikey, key: X-Key, value: k, addTo: queryParams }
environments:
  name: Base Environment
  data: { team: core, base: "http://base", GLOBAL_HEADERS: { Accept: "*/*" } }
  subEnvironments:
    - { name: Prod, meta: { sortKey: 2 }, data: { base: "http://prod" } }
    - { name: Dev, meta: { sortKey: 1 }, data: { base: "http://dev", GLOBAL_HEADERS: { X-Y: "1" } } }
"#;

    #[test]
    fn folders_nest_and_requests_land_in_their_folder() {
        let p = import(SAMPLE).unwrap();
        assert_eq!(p.name, "Shop");
        let names: Vec<_> = p.folders.iter().map(|f| (f.name.as_str(), f.parent)).collect();
        assert_eq!(names, [("Root", None), ("Inner", Some(0))]);
        assert_eq!(p.folders[1].requests[0].name, "Deep");
        assert_eq!(p.folders[0].requests[0].name, "Health");
        let root: Vec<_> = p.requests.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(root, ["First"], "the WebSocket item is skipped");
    }

    #[test]
    fn templates_auth_and_bodies_map() {
        let p = import(SAMPLE).unwrap();
        let (root, deep, health) = (&p.folders[0], &p.folders[1].requests[0], &p.folders[0].requests[0]);
        assert_eq!(root.headers[0].value, "{{team}}");
        assert_eq!(root.auth, Auth::Bearer("{{token}}".into()));
        assert_eq!((deep.url.as_str(), deep.method.as_str()), ("{{base}}/deep", "POST"));
        assert_eq!(deep.auth, Auth::None);
        let Body::FormData(fields) = &deep.body else {
            panic!("{:?}", deep.body)
        };
        assert_eq!(fields.len(), 2, "the file field is dropped");
        assert!(!fields[1].enabled, "a disabled row stays, switched off");
        assert_eq!(health.url, "{{base}}/health");
        assert_eq!(
            health.query[0].value, "{% prompt 'a', 'b' %}",
            "other tags are left as written"
        );
        assert!(matches!(&health.auth, Auth::Basic(b) if b.username == "u" && b.password == "p"));
        assert_eq!(health.body, Body::Json(r#"{"a": "{{base}}"}"#.into()));
        let first = &p.requests[0];
        assert_eq!((first.query[0].key.as_str(), first.auth.clone()), ("X-Key", Auth::None));
    }

    #[test]
    fn response_tags_become_response_functions() {
        let tag = |filter: &str, rest: &str| format!("{{% response 'body', 'req_1', '{filter}', {rest} %}}");
        let file = |value: &str| {
            format!(
                "type: collection.insomnia.rest/5.0\nname: P\ncollection:\n  - name: Token request\n    meta: {{id: req_1}}\n    method: POST\n  - name: Use\n    meta: {{id: req_2}}\n    method: GET\n    authentication: {{type: bearer, token: \"{value}\"}}\n"
            )
        };
        let token = |v: &str| match import(&file(v)).unwrap().requests[1].auth.clone() {
            Auth::Bearer(t) => t,
            a => panic!("{a:?}"),
        };
        let f = "b64::JC5hY2Nlc3NfdG9rZW4=::46b"; // $.access_token
        assert_eq!(
            token(&tag(f, "'when-expired', _.token_expires")),
            "{{ $response('Token request', '$.access_token', 'token_expires') }}"
        );
        assert_eq!(
            token(&format!("Bearer {}", tag(f, "'when-expired', 60"))),
            "Bearer {{ $response('Token request', '$.access_token', '60') }}"
        );
        assert_eq!(
            token(&tag(f, "'never', 0")),
            "{{ $response('Token request', '$.access_token') }}"
        );
        assert_eq!(
            token(&tag(f, "'always', 0")),
            "{{ $response('Token request', '$.access_token', '0') }}"
        );
        // XPath ('/a' in base64), an unknown request and other tags stay as written.
        let xpath = tag("b64::L2E=::46b", "'never', 0");
        assert_eq!(token(&xpath), xpath);
        let unknown = tag(f, "'never', 0").replace("req_1", "req_9");
        assert_eq!(token(&unknown), unknown);
    }

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(b64_decode("JC5hY2Nlc3NfdG9rZW4=").as_deref(), Some("$.access_token"));
        assert_eq!(b64_decode("YQ==").as_deref(), Some("a"));
        assert_eq!(b64_decode("YWI").as_deref(), Some("ab"));
        assert_eq!(b64_decode("a$"), None);
    }

    #[test]
    fn sub_environments_hold_base_plus_their_own_values() {
        let p = import(SAMPLE).unwrap();
        let names: Vec<_> = p.environments.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Dev", "Prod"], "in Insomnia's order");
        assert_eq!(p.active_env, "Dev");
        let dev = &p.environments[0].values;
        assert_eq!(
            (dev["base"].as_str(), dev["team"].as_str()),
            (Some("http://dev"), Some("core"))
        );
        assert!(
            dev.get("GLOBAL_HEADERS").is_none(),
            "global headers are not kept as variables"
        );
        // They become project headers with their literal values.
        let headers: Vec<_> = p.headers.iter().map(|h| (h.key.as_str(), h.value.as_str())).collect();
        assert_eq!(headers, [("Accept", "*/*"), ("X-Y", "1")]);
    }

    #[test]
    fn other_files_are_refused() {
        assert_eq!(
            import("type: environment.insomnia.rest/5.0"),
            Err(ImportError::NotAProject)
        );
        assert_eq!(import("[1, 2]"), Err(ImportError::NotAProject));
        assert_eq!(import("just text"), Err(ImportError::NotAProject));
        assert!(matches!(import("a: [unclosed"), Err(ImportError::Invalid(_))));
        let minimal = import("type: spec.insomnia.rest/5.0\nname: x").unwrap();
        assert!(minimal.requests.is_empty() && minimal.environments.is_empty());
    }
}

#[cfg(test)]
mod real_export {
    use super::*;

    #[test]
    fn folded_multiline_tag_in_a_folder() {
        let y = r#"
type: collection.insomnia.rest/5.0
name: P
collection:
  - name: Auth
    meta: { id: fld_1, sortKey: 1 }
    children:
      - url: https://admin.rac{{ _.totvs_environment
          }}.totvs.app/totvs.rac/connect/token
        name: Token de acesso
        method: POST
        meta:
          id: req_65ddc6742e9d4c33930acf2c6a6dfc20
          sortKey: -1678872614560.75
  - name: Use
    method: GET
    url: http://x
    meta: { id: req_2 }
    authentication:
      type: bearer
      token: "{% response 'body', 'req_65ddc6742e9d4c33930acf2c6a6dfc20',
        'b64::JC5hY2Nlc3NfdG9rZW4=::46b', 'when-expired',
        _.token_expires %}"
      prefix: ""
"#;
        let p = import(y).unwrap();
        assert_eq!(
            p.requests[0].auth,
            Auth::Bearer("{{ $response('Token de acesso', '$.access_token', 'token_expires') }}".into())
        );
    }
}
