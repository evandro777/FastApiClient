//! Turns a model `Request` into a wire request (`prepare`, pure and unit-testable) and sends it (`send`, blocking: call
//! it from a thread).
use std::{
    fmt,
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
    time::{Duration, Instant},
};

use chrono::Local;
use serde_json::Value;
use ureq::{SendBody, http};

use crate::{
    chain::{Lookup, Responses},
    model::{Auth, Body, Folder, KeyValue, Project, Request, new_id},
    resolve::{date_expr, effective_auth, merge_headers, substitute_with},
};

/// Larger responses are cut here so a huge download cannot exhaust memory or stall the UI.
pub const MAX_BODY: usize = 1024 * 1024;

/// The UI maps each variant to a translated message; `Display` is for logs and tests only.
#[derive(Debug)]
pub enum Error {
    Url,
    Method,
    /// Name of the header that is not valid on the wire.
    Header(String),
    /// Body file that could not be opened, with the reason.
    BodyFile(String),
    /// OAuth2 token handling is not wired in yet.
    OAuthPending,
    Network(String),
    /// A request that `{{ $response(...) }}` needs failed: its name and why.
    Dependency(String, String),
    /// Requests that need each other's responses, in a loop: the name where it was cut.
    Cycle(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Url => write!(f, "invalid URL"),
            Error::Method => write!(f, "invalid HTTP method"),
            Error::Header(h) => write!(f, "invalid header {h}"),
            Error::BodyFile(e) => write!(f, "cannot read body file {e}"),
            Error::OAuthPending => write!(f, "OAuth2 is not available yet"),
            Error::Network(e) => write!(f, "{e}"),
            Error::Dependency(n, e) => write!(f, "request {n} failed: {e}"),
            Error::Cycle(n) => write!(f, "request {n} needs its own response"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, PartialEq)]
pub enum Payload {
    None,
    Bytes(Vec<u8>),
    /// Opened and streamed at send time, so a big file is never held in memory.
    File(PathBuf),
}

#[derive(Debug)]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub payload: Payload,
    /// `{{variables}}` that the active environment does not define; the UI should warn about them.
    pub missing: Vec<String>,
}

impl Prepared {
    /// `host` or `host:port` of the URL, without any user:password; empty when there is none. For the "send a file"
    /// confirmation, which must name where the file goes.
    pub fn host(&self) -> String {
        let Ok(uri) = self.url.parse::<http::Uri>() else {
            return String::new();
        };
        match (uri.host(), uri.port_u16()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => String::new(),
        }
    }
}

pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    /// At most `MAX_BODY` bytes.
    pub body: Vec<u8>,
    pub truncated: bool,
    pub elapsed: Duration,
}

struct Vars<'a> {
    project: &'a Project,
    env: &'a Value,
    cache: &'a Responses,
    /// False on the second pass, once every needed response has been fetched: a value just fetched is used as is.
    check_age: bool,
    missing: Vec<String>,
    /// Requests whose cached response is absent or expired.
    stale: Vec<String>,
}

impl Vars<'_> {
    fn sub(&mut self, text: &str) -> String {
        let now = Local::now().fixed_offset();
        let (project, env, cache, check_age, stale) =
            (self.project, self.env, self.cache, self.check_age, &mut self.stale);
        let (out, missing) = substitute_with(text, self.env, &mut |name| {
            if name.starts_with("$response") {
                match cache.lookup(&project.id, env, name, check_age) {
                    Lookup::Value(v) => Some(v),
                    // An unknown request is "undefined"; only a real one is worth fetching.
                    Lookup::Stale(n) => {
                        if project.find_request(&n).is_some() && !stale.contains(&n) {
                            stale.push(n);
                        }
                        None
                    }
                    Lookup::Bad => None,
                }
            } else {
                date_expr(name, now)
            }
        });
        for m in missing {
            if !self.missing.contains(&m) {
                self.missing.push(m);
            }
        }
        out
    }

    fn pairs(&mut self, list: &[KeyValue]) -> Vec<(String, String)> {
        list.iter()
            .filter(|kv| kv.enabled && !kv.key.is_empty())
            .map(|kv| (self.sub(&kv.key), self.sub(&kv.value)))
            .collect()
    }
}

fn has(headers: &[(String, String)], name: &str) -> bool {
    headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
}

fn encode(s: &str) -> String {
    s.bytes().fold(String::with_capacity(s.len()), |mut out, b| {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
        out
    })
}

fn form_urlencoded(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    input
        .chunks(3)
        .flat_map(|c| {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            (0..4).map(move |i| {
                // 3 input bytes give 4 symbols; the last group is padded with `=`.
                if i > c.len() {
                    '='
                } else {
                    T[((n >> (18 - 6 * i)) & 63) as usize] as char
                }
            })
        })
        .collect()
}

/// What `send` will put on the wire, curl `-v` style, for the Request tab. Words are not translated: it is protocol text.
/// shortcut: ureq's own Host/Accept-Encoding are not listed; read them from ureq if users need them.
pub fn request_log(p: &Prepared) -> String {
    let mut out = format!("> {} {}\n", p.method, p.url);
    for (k, v) in &p.headers {
        out.push_str(&format!("> {k}: {v}\n"));
    }
    match &p.payload {
        Payload::None => {}
        Payload::Bytes(b) => {
            out.push_str(&format!("> Content-Length: {}\n\n", b.len()));
            out.push_str(&String::from_utf8_lossy(&b[..b.len().min(MAX_BODY)]));
        }
        Payload::File(f) => out.push_str(&format!("\n@{}", f.display())),
    }
    out
}

/// Re-indents a JSON object/array for display. Works on the text (not a parsed `Value`) so key order and number
/// digits stay exactly as the server sent them. `None` when it is not valid JSON or not an object/array.
pub fn pretty_json(s: &str) -> Option<String> {
    let t = s.trim();
    if !(t.starts_with('{') || t.starts_with('[')) || serde_json::from_str::<Value>(t).is_err() {
        return None;
    }
    let mut out = String::with_capacity(t.len() * 2);
    let (mut depth, mut in_str) = (0usize, false);
    let mut it = t.chars().peekable();
    let nl = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = it.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                out.extend(it.next());
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
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
    Some(out)
}

enum Xml<'a> {
    Open(&'a str),
    Close(&'a str),
    /// Self-closing tags, declarations, comments, CDATA and DOCTYPE: one line each, no nesting.
    Leaf(&'a str),
    Text(&'a str),
}

/// Splits XML into tags and trimmed text runs; `None` when a tag, comment or CDATA is not terminated.
/// shortcut: a DOCTYPE internal subset containing '>' is not supported, add it if a real API sends one.
fn xml_tokens(t: &str) -> Option<Vec<Xml<'_>>> {
    let mut out = vec![];
    let mut i = 0;
    while i < t.len() {
        let rest = &t[i..];
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = rest[..end].trim();
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
    let t = s.trim();
    if !t.starts_with('<') {
        return None;
    }
    let tokens = xml_tokens(t)?;
    let mut out = String::with_capacity(t.len() * 2);
    let mut stack: Vec<&str> = vec![];
    let mut elements = 0;
    let mut i = 0;
    let line = |out: &mut String, depth: usize, parts: &[&str]| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&"  ".repeat(depth));
        parts.iter().for_each(|p| out.push_str(p));
    };
    while i < tokens.len() {
        match &tokens[i] {
            Xml::Open(tag) => {
                elements += 1;
                let name = xml_name(tag);
                // `<n>text</n>` and `<n></n>` stay on one line.
                match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(Xml::Text(text)), Some(Xml::Close(c))) if xml_name(c) == name => {
                        line(&mut out, stack.len(), &[tag, text, c]);
                        i += 3;
                        continue;
                    }
                    (Some(Xml::Close(c)), _) if xml_name(c) == name => {
                        line(&mut out, stack.len(), &[tag, c]);
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
                line(&mut out, stack.len(), &[tag]);
                stack.push(name);
            }
            Xml::Close(tag) => {
                if stack.pop() != Some(xml_name(tag)) {
                    return None;
                }
                line(&mut out, stack.len(), &[tag]);
            }
            Xml::Leaf(tag) => {
                elements += usize::from(!tag.starts_with("<!") && !tag.starts_with("<?"));
                line(&mut out, stack.len(), &[tag]);
            }
            Xml::Text(text) => {
                if stack.is_empty() {
                    return None;
                }
                line(&mut out, stack.len(), &[text]);
            }
        }
        i += 1;
    }
    (stack.is_empty() && elements > 0).then_some(out)
}

/// Removes `//` and `/* */` comments outside strings. Trailing commas are not handled: shortcut, add it if users
/// paste JSON5-style bodies.
fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    let mut in_str = false;
    while let Some(c) = it.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                out.extend(it.next());
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, it.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => while it.next_if(|&n| n != '\n').is_some() {},
            ('/', Some('*')) => {
                it.next();
                let mut prev = ' ';
                for n in it.by_ref() {
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

fn multipart(boundary: &str, parts: &[(String, String)]) -> Vec<u8> {
    let mut out = String::new();
    for (k, v) in parts {
        // A quote or line break in the name would break out of the header line.
        let name = k.replace('"', "%22").replace(['\r', '\n'], "");
        out.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{v}\r\n"
        ));
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    out.into_bytes()
}

/// `folders` are the request's enclosing folders, outermost first; empty for a request directly under the project.
pub fn prepare(project: &Project, folders: &[&Folder], request: &Request) -> Result<Prepared, Error> {
    prepare_with(project, folders, request, &Responses::default(), false).map(|(p, _)| p)
}

/// `prepare` that also resolves `{{ $response(...) }}` from `cache`; the names of the requests to (re)fetch first
/// come back with it (see `chain::prepare`).
pub fn prepare_with(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    check_age: bool,
) -> Result<(Prepared, Vec<String>), Error> {
    let mut v = Vars {
        project,
        env: project.active_values(),
        cache,
        check_age,
        missing: vec![],
        stale: vec![],
    };

    let method = request.method.trim().to_ascii_uppercase();
    http::Method::from_bytes(method.as_bytes()).map_err(|_| Error::Method)?;

    // A fragment is never sent, and `http::Uri` rejects it.
    let mut url = v.sub(request.url.split('#').next().unwrap_or(""));
    let query = form_urlencoded(&v.pairs(&request.query));
    if !query.is_empty() {
        if !url.contains('?') {
            url.push('?');
        } else if !url.ends_with(['?', '&']) {
            url.push('&');
        }
        url.push_str(&query);
    }

    let header_chain: Vec<&[KeyValue]> = std::iter::once(project.headers.as_slice())
        .chain(folders.iter().map(|f| f.headers.as_slice()))
        .chain(std::iter::once(request.headers.as_slice()))
        .collect();
    let mut headers = v.pairs(&merge_headers(&header_chain));

    // An Authorization header written by hand wins over the Auth tab.
    if !has(&headers, "authorization") {
        let auth_chain: Vec<&Auth> = std::iter::once(&project.auth)
            .chain(folders.iter().map(|f| &f.auth))
            .chain(std::iter::once(&request.auth))
            .collect();
        let value = match effective_auth(&auth_chain) {
            Auth::Basic(b) => {
                let credentials = format!("{}:{}", v.sub(&b.username), v.sub(&b.password));
                Some(format!("Basic {}", base64(credentials.as_bytes())))
            }
            Auth::Bearer(t) => Some(format!("Bearer {}", v.sub(t))),
            Auth::OAuth2(_) => return Err(Error::OAuthPending),
            Auth::None | Auth::Inherit => None,
        };
        headers.extend(value.map(|val| ("Authorization".to_string(), val)));
    }

    let (payload, content_type) = match &request.body {
        Body::None => (Payload::None, None),
        // Comments are stripped before substitution so a variable's value is never taken for a comment.
        Body::Json(t) => (
            Payload::Bytes(v.sub(&strip_jsonc(t)).into_bytes()),
            Some("application/json".to_string()),
        ),
        Body::Xml(t) => (
            Payload::Bytes(v.sub(t).into_bytes()),
            Some("application/xml".to_string()),
        ),
        Body::FormUrl(kv) => (
            Payload::Bytes(form_urlencoded(&v.pairs(kv)).into_bytes()),
            Some("application/x-www-form-urlencoded".to_string()),
        ),
        Body::FormData(kv) => {
            let boundary = format!("----FastApiClient{}", new_id());
            let bytes = multipart(&boundary, &v.pairs(kv));
            // The boundary must match the body, so a hand-written Content-Type cannot stay.
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            (
                Payload::Bytes(bytes),
                Some(format!("multipart/form-data; boundary={boundary}")),
            )
        }
        Body::File(p) => (
            Payload::File(PathBuf::from(v.sub(p))),
            Some("application/octet-stream".to_string()),
        ),
    };
    if let Some(ct) = content_type.filter(|_| !has(&headers, "content-type")) {
        headers.push(("Content-Type".into(), ct));
    }
    // Headers are already merged project → folders → request, so a User-Agent set at any level wins over this default.
    if !has(&headers, "user-agent") {
        headers.push((
            "User-Agent".into(),
            concat!("FastApiClient/", env!("CARGO_PKG_VERSION")).into(),
        ));
    }

    Ok((
        Prepared {
            method,
            url,
            headers,
            payload,
            missing: v.missing,
        },
        v.stale,
    ))
}

/// ureq refuses a body on these verbs (`BodyNotAllowed`) unless the request starts from its own builder and is told to
/// force it. Insomnia exports have many GETs with a JSON body, and those must go out as written.
fn forced_builder(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
) -> Option<ureq::RequestBuilder<ureq::typestate::WithBody>> {
    Some(match method {
        "GET" => agent.get(url).force_send_body(),
        "HEAD" => agent.head(url).force_send_body(),
        "DELETE" => agent.delete(url).force_send_body(),
        "OPTIONS" => agent.options(url).force_send_body(),
        "TRACE" => agent.trace(url).force_send_body(),
        _ => return None,
    })
}

pub fn send(p: Prepared) -> Result<Response, Error> {
    let started = Instant::now();
    let mut builder = http::Request::builder().method(p.method.as_str()).uri(p.url.as_str());
    for (k, val) in &p.headers {
        let name = http::HeaderName::from_bytes(k.as_bytes()).map_err(|_| Error::Header(k.clone()))?;
        let value = http::HeaderValue::from_str(val).map_err(|_| Error::Header(k.clone()))?;
        builder = builder.header(name, value);
    }
    let has_body = !matches!(p.payload, Payload::None);
    let body = match p.payload {
        Payload::None => SendBody::none(),
        Payload::Bytes(bytes) => {
            builder = builder.header("content-length", bytes.len());
            SendBody::from_owned_reader(Cursor::new(bytes))
        }
        Payload::File(path) => {
            let file_error = |e: std::io::Error| Error::BodyFile(format!("{}: {e}", path.display()));
            let file = File::open(&path).map_err(file_error)?;
            builder = builder.header("content-length", file.metadata().map_err(file_error)?.len());
            SendBody::from_owned_reader(file)
        }
    };
    let request = builder.body(body).map_err(|_| Error::Url)?;

    let config = ureq::Agent::config_builder()
        // 4xx/5xx are answers to show, not failures.
        .http_status_as_error(false)
        // ureq refuses anything but the standard verbs otherwise (PURGE, PROPFIND, ...).
        .allow_non_standard_methods(true)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = match (has_body, forced_builder(&agent, &p.method, &p.url)) {
        (true, Some(mut forced)) => {
            let (parts, body) = request.into_parts();
            for (name, value) in &parts.headers {
                forced = forced.header(name, value);
            }
            forced.send(body)
        }
        _ => agent.run(request),
    }
    .map_err(|e| Error::Network(e.to_string()))?;

    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let mut body = Vec::new();
    // One byte more than the cap tells "exactly the cap" from "more than the cap".
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| Error::Network(e.to_string()))?;
    let truncated = body.len() > MAX_BODY;
    body.truncate(MAX_BODY);

    Ok(Response {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
        truncated,
        elapsed: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_names_where_a_request_goes_without_credentials() {
        let at = |url: &str| {
            super::Prepared {
                method: "POST".into(),
                url: url.into(),
                headers: vec![],
                payload: super::Payload::None,
                missing: vec![],
            }
            .host()
        };
        assert_eq!(at("https://evil.example/up?a=1"), "evil.example");
        assert_eq!(at("http://u:secret@10.0.0.5:8080/x"), "10.0.0.5:8080");
        assert_eq!(at("not a url"), "");
        assert_eq!(at("/relative"), "");
    }

    #[test]
    fn request_log_lists_line_headers_and_body() {
        let mut p = super::Prepared {
            method: "POST".into(),
            url: "http://x/y?a=1".into(),
            headers: vec![("Content-Type".into(), "application/json".into())],
            payload: super::Payload::Bytes(b"{\"a\":1}".to_vec()),
            missing: vec![],
        };
        assert_eq!(
            super::request_log(&p),
            "> POST http://x/y?a=1\n> Content-Type: application/json\n> Content-Length: 7\n\n{\"a\":1}"
        );
        p.payload = super::Payload::File("/tmp/f.bin".into());
        assert!(super::request_log(&p).ends_with("\n\n@/tmp/f.bin"));
        p.payload = super::Payload::None;
        assert_eq!(
            super::request_log(&p),
            "> POST http://x/y?a=1\n> Content-Type: application/json\n"
        );
    }

    #[test]
    fn pretty_xml_indents_and_rejects_malformed() {
        let src = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://x"><s:Body><r a="1>2"><n>Ana</n><e/><!-- c --><![CDATA[a<b]]><z></z></r>
            </s:Body></s:Envelope>"#;
        let want = "<?xml version=\"1.0\"?>\n<s:Envelope xmlns:s=\"http://x\">\n  <s:Body>\n    <r a=\"1>2\">\n      <n>Ana</n>\n      <e/>\n      <!-- c -->\n      <![CDATA[a<b]]>\n      <z></z>\n    </r>\n  </s:Body>\n</s:Envelope>";
        assert_eq!(super::pretty_xml(src).as_deref(), Some(want));
        assert_eq!(super::pretty_xml("<a><b></a></b>"), None);
        assert_eq!(super::pretty_xml("<a><b>"), None);
        assert_eq!(super::pretty_xml("<a"), None);
        assert_eq!(super::pretty_xml("plain text"), None);
        assert_eq!(super::pretty_xml("<!-- only a comment -->"), None);
    }

    #[test]
    fn pretty_json_keeps_order_numbers_and_strings() {
        let src = r#"{"b":12345678901234567890123,"a":[1,{"s":"x{,:\"}"}],"e":{},"l":[ ]}"#;
        let want = "{\n  \"b\": 12345678901234567890123,\n  \"a\": [\n    1,\n    {\n      \"s\": \"x{,:\\\"}\"\n    }\n  ],\n  \"e\": {},\n  \"l\": []\n}";
        assert_eq!(super::pretty_json(src).as_deref(), Some(want));
        assert_eq!(super::pretty_json("{\"a\":"), None);
        assert_eq!(super::pretty_json("42"), None);
        assert_eq!(super::pretty_json("plain text"), None);
    }

    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    use serde_json::json;

    use super::*;
    use crate::model::{Basic, Environment};

    /// One-shot server that answers 200 with the raw request it received as the body. Returns its base URL.
    fn echo() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut raw, mut length) = (String::new(), 0usize);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
                raw.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            raw.push_str(&String::from_utf8_lossy(&body));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",
                raw.len()
            )
            .unwrap();
        });
        url
    }

    fn roundtrip(project: &Project, folders: &[&Folder], request: &Request) -> String {
        let response = send(prepare(project, folders, request).unwrap()).unwrap();
        assert_eq!(response.status, 200);
        String::from_utf8(response.body).unwrap()
    }

    fn project_with_host(host: &str) -> Project {
        Project {
            environments: vec![Environment {
                name: "local".into(),
                values: json!({ "host": host, "token": "t0k" }),
            }],
            active_env: "local".into(),
            ..Project::default()
        }
    }

    #[test]
    fn query_headers_auth_and_variables() {
        let mut project = project_with_host(&echo());
        project.headers = vec![KeyValue::new("X-A", "project"), KeyValue::new("X-P", "p")];
        project.auth = Auth::Bearer("{{token}}".into());
        let outer = Folder {
            headers: vec![KeyValue::new("x-a", "outer"), KeyValue::new("x-o", "o")],
            auth: Auth::Bearer("outer-token".into()),
            ..Folder::default()
        };
        // Nested folders chain: the inner one wins for `x-a`, and its Inherit auth falls back to the outer folder's.
        let folder = Folder {
            headers: vec![KeyValue::new("x-a", "folder")],
            ..Folder::default()
        };
        let request = Request {
            method: "post".into(),
            url: "{{host}}/path?x=1#frag".into(),
            query: vec![KeyValue::new("a b", "c&d"), KeyValue::new("", "skipped")],
            ..Request::default()
        };
        let raw = roundtrip(&project, &[&outer, &folder], &request).to_ascii_lowercase();
        assert!(raw.starts_with("post /path?x=1&a%20b=c%26d http/1.1"), "{raw}");
        assert!(
            raw.contains("x-a: folder\r\n") && !raw.contains("x-a: project"),
            "{raw}"
        );
        assert!(raw.contains("x-p: p\r\n") && raw.contains("x-o: o\r\n"), "{raw}");
        assert!(raw.contains("authorization: bearer outer-token\r\n"), "{raw}");
    }

    #[test]
    fn a_get_can_carry_a_body() {
        let project = project_with_host(&echo());
        let request = Request {
            method: "get".into(),
            url: "{{host}}/".into(),
            body: Body::Json(r#"{"a": 1}"#.into()),
            ..Request::default()
        };
        let raw = roundtrip(&project, &[], &request);
        assert!(raw.starts_with("GET / HTTP/1.1"), "{raw}");
        assert!(raw.ends_with("\r\n\r\n{\"a\": 1}"), "{raw}");
    }

    #[test]
    fn basic_auth_and_custom_method() {
        let project = project_with_host(&echo());
        let request = Request {
            method: "PURGE".into(),
            url: "{{host}}/".into(),
            auth: Auth::Basic(Basic {
                username: "user".into(),
                password: "pass".into(),
            }),
            ..Request::default()
        };
        let raw = roundtrip(&project, &[], &request);
        assert!(raw.starts_with("PURGE / HTTP/1.1"), "{raw}");
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: basic dxnlcjpwyxnz\r\n"),
            "{raw}"
        );
    }

    #[test]
    fn user_agent_defaults_to_app_name_and_can_be_overridden() {
        let project = project_with_host(&echo());
        let mut request = Request {
            url: "{{host}}/".into(),
            ..Request::default()
        };
        let raw = roundtrip(&project, &[], &request).to_ascii_lowercase();
        let default = format!("user-agent: fastapiclient/{}\r\n", env!("CARGO_PKG_VERSION"));
        assert!(raw.contains(&default), "{raw}");

        request.headers.push(KeyValue {
            key: "user-agent".into(),
            value: "mine/1".into(),
            description: String::new(),
            enabled: true,
        });
        let project = project_with_host(&echo());
        let raw = roundtrip(&project, &[], &request).to_ascii_lowercase();
        assert!(
            raw.contains("user-agent: mine/1\r\n") && !raw.contains("fastapiclient/"),
            "{raw}"
        );
    }

    #[test]
    fn jsonc_body_loses_comments_but_keeps_urls_in_strings() {
        let project = project_with_host(&echo());
        let request = Request {
            method: "POST".into(),
            url: "{{host}}/".into(),
            body: Body::Json("{\n// note\n\"u\": \"http://x/*y*/\" /* c */, \"t\": \"{{token}}\"}".into()),
            ..Request::default()
        };
        let raw = roundtrip(&project, &[], &request);
        assert!(
            raw.to_ascii_lowercase().contains("content-type: application/json\r\n"),
            "{raw}"
        );
        let body = raw.split("\r\n\r\n").nth(1).unwrap();
        assert_eq!(body, "{\n\n\"u\": \"http://x/*y*/\" , \"t\": \"t0k\"}");
    }

    #[test]
    fn form_url_and_multipart() {
        let project = project_with_host(&echo());
        let off = KeyValue {
            enabled: false,
            ..KeyValue::new("off", "x")
        };
        let mut request = Request {
            method: "POST".into(),
            url: "{{host}}/".into(),
            body: Body::FormUrl(vec![KeyValue::new("k", "a b"), off]),
            ..Request::default()
        };
        let raw = roundtrip(&project, &[], &request);
        assert!(raw.ends_with("\r\n\r\nk=a%20b"), "{raw}");

        let project = project_with_host(&echo());
        request.body = Body::FormData(vec![KeyValue::new("f\"x", "v1")]);
        // A hand-written Content-Type must not survive: its boundary would not match the body.
        request.headers = vec![KeyValue::new("Content-Type", "text/plain")];
        let raw = roundtrip(&project, &[], &request);
        assert!(!raw.contains("text/plain"), "{raw}");
        assert!(raw.contains("multipart/form-data; boundary=----FastApiClient"), "{raw}");
        assert!(raw.contains("name=\"f%22x\"\r\n\r\nv1\r\n--"), "{raw}");
    }

    #[test]
    fn file_body_is_streamed_with_its_length() {
        let path = std::env::temp_dir().join(format!("fac-body-{}", new_id()));
        std::fs::write(&path, "file-content").unwrap();
        let project = project_with_host(&echo());
        let request = Request {
            method: "PUT".into(),
            url: "{{host}}/".into(),
            body: Body::File(path.to_string_lossy().into_owned()),
            ..Request::default()
        };
        let raw = roundtrip(&project, &[], &request);
        std::fs::remove_file(path).unwrap();
        assert!(raw.to_ascii_lowercase().contains("content-length: 12\r\n"), "{raw}");
        assert!(raw.ends_with("\r\n\r\nfile-content"), "{raw}");
    }

    #[test]
    fn errors_before_the_network() {
        let project = Project::default();
        let bad_method = Request {
            method: "GE T".into(),
            ..Request::default()
        };
        assert!(matches!(prepare(&project, &[], &bad_method), Err(Error::Method)));

        let missing = Request {
            url: "http://{{nope}}/".into(),
            body: Body::File("/definitely/not/here".into()),
            ..Request::default()
        };
        let prepared = prepare(&project, &[], &missing).unwrap();
        assert_eq!(prepared.missing, ["nope"]);
        assert!(matches!(send(prepared), Err(Error::BodyFile(_))));

        let oauth = Request {
            auth: Auth::OAuth2(Default::default()),
            ..Request::default()
        };
        assert!(matches!(prepare(&project, &[], &oauth), Err(Error::OAuthPending)));
    }

    #[test]
    fn base64_pads_every_length() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
    }
}
