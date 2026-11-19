//! Turns a model `Request` into a wire request (`prepare`, pure and unit-testable) and sends it (`send`, blocking: call
//! it from a thread).
use std::{
    fmt,
    fs::File,
    io::{Cursor, Read},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::Local;
use serde_json::Value;
use ureq::{RequestExt, SendBody, http};

use crate::{
    chain::{Lookup, Responses},
    model::{Auth, Body, Folder, KeyValue, Project, Request, new_id},
    resolve::{Prompt, date_expr, effective_auth, substitute_with},
};

/// Larger responses are cut here so a huge download cannot exhaust memory or stall the UI.
pub const MAX_BODY: usize = 1024 * 1024;

use crate::format::strip_jsonc;
pub use crate::format::{
    AUTO_TEXT_LIMIT, FormatError, pretty_json, pretty_json_limited, pretty_xml, pretty_xml_limited, small_text,
};

/// The UI maps each variant to a translated message; `Display` is for logs and tests only.
#[derive(Debug)]
pub enum Error {
    Cancelled,
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
    /// A successful dependency did not supply a complete JSON response.
    DependencyJson(String),
    CacheLimit,
    /// Requests that need each other's responses, in a loop: the name where it was cut.
    Cycle(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "cancelled"),
            Error::Url => write!(f, "invalid URL"),
            Error::Method => write!(f, "invalid HTTP method"),
            Error::Header(h) => write!(f, "invalid header {h}"),
            Error::BodyFile(e) => write!(f, "cannot read body file {e}"),
            Error::OAuthPending => write!(f, "OAuth2 is not available yet"),
            Error::Network(e) => write!(f, "{e}"),
            Error::Dependency(n, e) => write!(f, "request {n} failed: {e}"),
            Error::DependencyJson(n) => write!(f, "request {n} did not return a complete JSON response"),
            Error::CacheLimit => write!(f, "response dependencies exceed the cache limit"),
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

/// Stops the active socket for one Send, including dependency refreshes.
#[derive(Clone, Default)]
pub struct CancelHandle {
    cancelled: Arc<AtomicBool>,
    agent: Arc<Mutex<Option<ureq::Agent>>>,
}

impl CancelHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let agent = self.agent.lock().unwrap().clone();
        if let Some(agent) = agent {
            agent.shutdown();
        }
    }

    fn install(&self, agent: &ureq::Agent) -> Result<(), Error> {
        let mut active = self.agent.lock().unwrap();
        if self.cancelled.load(Ordering::SeqCst) {
            drop(active);
            agent.clone().shutdown();
            return Err(Error::Cancelled);
        }
        *active = Some(agent.clone());
        Ok(())
    }

    fn clear(&self) {
        self.agent.lock().unwrap().take();
    }
}

pub type PromptResolver<'a> = dyn FnMut(&Prompt) -> Result<String, Error> + 'a;

struct Vars<'a, 'p> {
    prompt: Option<&'p mut PromptResolver<'p>>,
    error: Option<Error>,
    project: &'a Project,
    env: &'a Value,
    cache: &'a Responses,
    /// Responses fetched during this Send are used even when their expiry is zero.
    check_age: &'a dyn Fn(&str) -> bool,
    missing: Vec<String>,
    /// Requests whose cached response is absent or expired.
    stale: Vec<String>,
    /// The project serialized once, for the cache's validity check (see `Responses::lookup_in`).
    snapshot: Option<Vec<u8>>,
}

impl Vars<'_, '_> {
    fn sub(&mut self, text: &str) -> String {
        if self.error.is_some() {
            return text.into();
        }
        let now = Local::now().fixed_offset();
        let (project, cache, check_age, stale) = (self.project, self.cache, self.check_age, &mut self.stale);
        let snapshot = &mut self.snapshot;
        let (out, missing) = substitute_with(text, self.env, &mut |name| {
            if self.error.is_some() {
                return None;
            }
            if let Some(spec) = Prompt::parse(name) {
                match self.prompt.as_mut()?(&spec) {
                    Ok(answer) => Some(answer),
                    Err(error) => {
                        self.error = Some(error);
                        None
                    }
                }
            } else if name.starts_with("$response") {
                let check_age = crate::resolve::args(name, "$response")
                    .and_then(|args| args.into_iter().next())
                    .is_none_or(|request| check_age(&request));
                let source =
                    snapshot.get_or_insert_with(|| serde_json::to_vec(project).expect("a project always serializes"));
                match cache.lookup_in(project, source, name, check_age) {
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
        if !matches!(p.payload, Payload::None) && k.eq_ignore_ascii_case("content-length") {
            continue;
        }
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
    prepare_interactive(project, folders, request, cache, |_| check_age, None)
}

/// A nonempty dependency list means this is an incomplete draft: fetch dependencies before sending.
pub fn prepare_interactive<'p>(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    check_age: impl Fn(&str) -> bool,
    prompt: Option<&'p mut PromptResolver<'p>>,
) -> Result<(Prepared, Vec<String>), Error> {
    let mut v = Vars {
        prompt,
        error: None,
        project,
        env: project.active_values(),
        cache,
        check_age: &check_age,
        missing: vec![],
        stale: vec![],
        snapshot: None,
    };

    let method = request.method.trim().to_ascii_uppercase();
    http::Method::from_bytes(method.as_bytes()).map_err(|_| Error::Method)?;

    // Resolve first: '#' can be a literal prompt argument or part of an environment's URL.
    let mut url = v.sub(&request.url);
    if let Some(fragment) = url.find('#') {
        url.truncate(fragment);
    }
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
    // Resolve names before choosing winners; overridden values must not prompt or fetch dependencies.
    let mut named: Vec<KeyValue> = vec![];
    for mut header in header_chain
        .iter()
        .flat_map(|list| list.iter())
        .filter(|header| header.enabled && !header.key.is_empty())
        .cloned()
    {
        header.key = v.sub(&header.key);
        named.retain(|old| !old.key.eq_ignore_ascii_case(&header.key));
        named.push(header);
    }
    if let Some(error) = v.error.take() {
        return Err(error);
    }
    // A missing header name can still override any inherited value, auth or content type.
    if !v.stale.is_empty() {
        return Ok((
            Prepared {
                method,
                url,
                headers: vec![],
                payload: Payload::None,
                missing: v.missing,
            },
            v.stale,
        ));
    }
    let mut headers: Vec<_> = named
        .into_iter()
        .filter(|header| {
            !(header.key.eq_ignore_ascii_case("content-length") && !matches!(request.body, Body::None)
                || header.key.eq_ignore_ascii_case("content-type") && matches!(request.body, Body::FormData(_)))
        })
        .map(|header| (header.key, v.sub(&header.value)))
        .collect();

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
            Auth::OAuth2(_) => return Err(v.error.take().unwrap_or(Error::OAuthPending)),
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

    if let Some(error) = v.error {
        return Err(error);
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

fn redirect_target(current: &url::Url, location: &str) -> Option<url::Url> {
    let mut next = current.join(location).ok()?;
    if next.origin() != current.origin() || !next.username().is_empty() || next.password().is_some() {
        return None;
    }
    next.set_fragment(None);
    Some(next)
}

pub fn send(p: Prepared) -> Result<Response, Error> {
    send_cancellable(p, &CancelHandle::default())
}

pub fn send_cancellable(p: Prepared, cancel: &CancelHandle) -> Result<Response, Error> {
    let started = Instant::now();
    let has_body = !matches!(p.payload, Payload::None);
    let mut builder = http::Request::builder().method(p.method.as_str()).uri(p.url.as_str());
    for (k, val) in &p.headers {
        // The actual byte/file length replaces imported or hand-written lengths.
        if has_body && k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let name = http::HeaderName::from_bytes(k.as_bytes()).map_err(|_| Error::Header(k.clone()))?;
        let value = http::HeaderValue::from_str(val).map_err(|_| Error::Header(k.clone()))?;
        builder = builder.header(name, value);
    }
    let body = match p.payload {
        Payload::None => SendBody::none(),
        Payload::Bytes(bytes) => {
            builder = builder.header("content-length", bytes.len());
            SendBody::from_owned_reader(Cursor::new(bytes))
        }
        Payload::File(path) => {
            let file_error = |e: std::io::Error| Error::BodyFile(format!("{}: {e}", path.display()));
            // Linux O_NONBLOCK: reject pipes before opening one can wait for a writer.
            let file = File::options()
                .read(true)
                .custom_flags(0o4000)
                .open(&path)
                .map_err(file_error)?;
            let metadata = file.metadata().map_err(file_error)?;
            if !metadata.is_file() {
                return Err(file_error(std::io::Error::other("not a regular file")));
            }
            builder = builder.header("content-length", metadata.len());
            SendBody::from_owned_reader(file)
        }
    };
    let request = builder.body(body).map_err(|_| Error::Url)?;

    let config = ureq::Agent::config_builder()
        // 4xx/5xx are answers to show, not failures.
        .http_status_as_error(false)
        // ureq refuses anything but the standard verbs otherwise (PURGE, PROPFIND, ...).
        .allow_non_standard_methods(true)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    cancel.install(&agent)?;
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
    .map_err(|e| {
        if cancel.cancelled.load(Ordering::SeqCst) {
            Error::Cancelled
        } else {
            Error::Network(e.to_string())
        }
    })?;

    let mut current = url::Url::parse(&p.url).map_err(|_| Error::Url)?;
    let mut method = p.method;
    let mut body_sent = has_body;
    for _ in 0..10 {
        let status = response.status().as_u16();
        if !matches!(status, 301 | 302 | 303 | 307 | 308)
            || (matches!(status, 307 | 308) && (body_sent || method == "DELETE"))
        {
            break;
        }
        let Some(next) = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| redirect_target(&current, location))
        else {
            break;
        };
        if method != "HEAD" && !matches!(status, 307 | 308) {
            method = "GET".into();
        }
        let mut redirected = http::Request::builder().method(method.as_str()).uri(next.as_str());
        for (name, value) in &p.headers {
            if !name.eq_ignore_ascii_case("cookie") && !name.eq_ignore_ascii_case("content-length") {
                redirected = redirected.header(name, value);
            }
        }
        // Keep one timeout budget for the entire redirect chain.
        let remaining = Duration::from_secs(60)
            .checked_sub(started.elapsed())
            .ok_or_else(|| Error::Network("request timed out".into()))?;
        drop(response);
        response = redirected
            .body(SendBody::none())
            .map_err(|_| Error::Url)?
            .with_agent(&agent)
            .configure()
            .timeout_global(Some(remaining))
            .run()
            .map_err(|e| {
                if cancel.cancelled.load(Ordering::SeqCst) {
                    Error::Cancelled
                } else {
                    Error::Network(e.to_string())
                }
            })?;
        current = next;
        body_sent = false;
    }

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
        .map_err(|e| {
            if cancel.cancelled.load(Ordering::SeqCst) {
                Error::Cancelled
            } else {
                Error::Network(e.to_string())
            }
        })?;
    let truncated = body.len() > MAX_BODY;
    body.truncate(MAX_BODY);

    cancel.clear();
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
    fn automatically_replaced_header_values_are_never_evaluated() {
        for body in [
            Body::FormData(vec![KeyValue::new("field", "value")]),
            Body::Json("{}".into()),
        ] {
            let mut project = Project::new("Replaced headers");
            project.requests.push(Request {
                name: "Unused".into(),
                url: "http://unused".into(),
                ..Request::default()
            });
            let mut request = Request {
                url: "http://unused".into(),
                body,
                headers: vec![KeyValue::new("Content-Length", "{{ $prompt('Unused') }}")],
                ..Request::default()
            };
            if matches!(request.body, Body::FormData(_)) {
                request
                    .headers
                    .push(KeyValue::new("Content-Type", "{{ $response('Unused', '$.type') }}"));
            }
            let (prepared, stale) = prepare_interactive(
                &project,
                &[],
                &request,
                &Responses::default(),
                |_| true,
                Some(&mut |_| panic!("discarded header prompted")),
            )
            .unwrap();
            assert!(stale.is_empty());
            assert!(prepared.missing.is_empty());
            assert!(
                !prepared
                    .headers
                    .iter()
                    .any(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            );
            if matches!(request.body, Body::FormData(_)) {
                assert!(
                    prepared.headers.iter().any(
                        |(key, value)| key == "Content-Type" && value.starts_with("multipart/form-data; boundary=")
                    )
                );
            }
        }
        let request = Request {
            url: "http://unused".into(),
            body: Body::Json("{}".into()),
            headers: vec![KeyValue::new("Content-Type", "{{ $prompt('Type') }}")],
            ..Request::default()
        };
        let (prepared, _) = prepare_interactive(
            &Project::new("Explicit type"),
            &[],
            &request,
            &Responses::default(),
            |_| true,
            Some(&mut |_| Ok("application/custom".into())),
        )
        .unwrap();
        assert!(
            prepared
                .headers
                .contains(&("Content-Type".into(), "application/custom".into()))
        );
    }

    #[test]
    fn redirects_convert_303_to_get_but_never_replay_a_body_on_307() {
        for status in [303, 307] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = std::thread::spawn(move || {
                let mut captures = vec![];
                for first in std::iter::once(true).chain((status == 303).then_some(false)) {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut headers = String::new();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = value.trim().parse().unwrap();
                        }
                        headers.push_str(&line);
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    captures.push((headers, body));
                    if first {
                        write!(stream, "HTTP/1.1 {status} Redirect\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    } else {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                            .unwrap();
                    }
                }
                captures
            });
            let request = Request {
                method: "POST".into(),
                url: format!("http://{address}/start"),
                body: Body::Json("{}".into()),
                ..Request::default()
            };
            let response = send(prepare(&Project::new("Redirect"), &[], &request).unwrap()).unwrap();
            assert_eq!(response.status, if status == 303 { 200 } else { 307 });
            let captures = worker.join().unwrap();
            assert_eq!(captures[0].1, b"{}");
            if status == 303 {
                assert!(captures[1].0.starts_with("GET /final HTTP/1.1\r\n"));
                assert!(captures[1].1.is_empty());
            } else {
                assert_eq!(captures.len(), 1);
            }
        }
    }

    #[test]
    fn redirects_keep_credentials_on_the_same_origin_and_stop_before_another_origin() {
        for same_host in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = std::thread::spawn(move || {
                let mut captures = vec![];
                for first in std::iter::once(true).chain((same_host).then_some(false)) {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut headers = String::new();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        headers.push_str(&line);
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                    }
                    captures.push(headers.to_ascii_lowercase());
                    if first {
                        let host = if same_host { "127.0.0.1" } else { "localhost" };
                        write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://{host}:{}/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", address.port()).unwrap();
                    } else {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                            .unwrap();
                    }
                }
                captures
            });
            let request = Request {
                url: format!("http://{address}/start"),
                auth: Auth::Bearer("secret".into()),
                headers: vec![KeyValue::new("X-Api-Key", "private")],
                ..Request::default()
            };
            let response = send(prepare(&Project::new("Redirect"), &[], &request).unwrap()).unwrap();
            assert_eq!(response.status, if same_host { 200 } else { 302 });
            let captures = worker.join().unwrap();
            assert!(captures[0].contains("authorization: bearer secret\r\n"));
            assert!(captures[0].contains("x-api-key: private\r\n"));
            assert_eq!(captures.len(), if same_host { 2 } else { 1 });
            if same_host {
                assert!(captures[1].contains("authorization: bearer secret\r\n"));
                assert!(captures[1].contains("x-api-key: private\r\n"));
            }
        }
    }

    #[test]
    fn redirect_locations_preserve_origin_and_resolve_relative_references() {
        let current = url::Url::parse("https://api.example/a/start?old=1").unwrap();
        for (location, expected) in [
            ("../final", "https://api.example/final"),
            ("/final#fragment", "https://api.example/final"),
            ("?new=1", "https://api.example/a/start?new=1"),
            ("//API.EXAMPLE:443/final", "https://api.example/final"),
        ] {
            assert_eq!(redirect_target(&current, location).unwrap().as_str(), expected);
        }
        for location in [
            "https://other.example/final",
            "//other.example/final",
            "http://api.example/final",
            "https://api.example:444/final",
            "https://user:password@api.example/final",
        ] {
            assert!(redirect_target(&current, location).is_none(), "{location}");
        }
    }

    #[test]
    fn header_variable_names_remain_case_sensitive() {
        let mut p = project_with_host(&echo());
        p.environments[0].values["Name"] = json!("X-One");
        p.environments[0].values["name"] = json!("X-Two");
        p.headers.push(KeyValue::new("{{Name}}", "one"));
        let request = Request {
            url: "{{host}}/".into(),
            headers: vec![KeyValue::new("{{name}}", "two")],
            ..Request::default()
        };
        let raw = roundtrip(&p, &[], &request).to_ascii_lowercase();
        assert!(raw.contains("x-one: one\r\n"), "{raw}");
        assert!(raw.contains("x-two: two\r\n"), "{raw}");
    }

    #[test]
    fn file_bodies_reject_nonregular_files_without_waiting() {
        let dir = std::env::temp_dir().join(format!("fac-body-{}", new_id()));
        std::fs::create_dir(&dir).unwrap();
        let fifo = dir.join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for path in [fifo, dir.clone(), PathBuf::from("/dev/zero")] {
                let result = send(Prepared {
                    method: "POST".into(),
                    url: "http://unused".into(),
                    headers: vec![],
                    payload: Payload::File(path),
                    missing: vec![],
                });
                assert!(matches!(result, Err(Error::BodyFile(e)) if e.contains("not a regular file")));
            }
            std::fs::remove_dir_all(dir).unwrap();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(3))
            .expect("nonregular body file blocked Send");
    }

    #[test]
    fn templated_header_names_follow_inheritance_on_the_wire() {
        for enabled in [true, false] {
            let mut p = project_with_host(&echo());
            p.environments[0].values["header"] = json!("X-Api-Key");
            p.environments[0].values["child"] = json!("x-api-key");
            p.environments[0].values["auth_header"] = json!("Authorization");
            p.headers = vec![
                KeyValue::new("X-Api-Key", "parent-key"),
                KeyValue::new("Authorization", "Bearer parent"),
            ];
            p.auth = Auth::Bearer("fallback".into());
            let folder = Folder {
                headers: vec![
                    KeyValue::new("{{header}}", "folder-key"),
                    KeyValue::new("{{auth_header}}", "Bearer folder"),
                ],
                ..Folder::default()
            };
            let mut key = KeyValue::new("{{child}}", "request-key");
            key.enabled = enabled;
            let request = Request {
                url: "{{host}}/".into(),
                headers: vec![key, KeyValue::new("authorization", "Bearer request")],
                ..Request::default()
            };
            let raw = roundtrip(&p, &[&folder], &request).to_ascii_lowercase();
            assert_eq!(raw.matches("x-api-key:").count(), 1, "{raw}");
            assert!(
                raw.contains(if enabled {
                    "x-api-key: request-key\r\n"
                } else {
                    "x-api-key: folder-key\r\n"
                }),
                "{raw}"
            );
            assert_eq!(raw.matches("authorization:").count(), 1, "{raw}");
            assert!(raw.contains("authorization: bearer request\r\n"), "{raw}");
            assert!(
                !raw.contains("parent-key")
                    && !raw.contains("bearer parent")
                    && !raw.contains("bearer folder")
                    && !raw.contains("fallback"),
                "{raw}"
            );
        }
    }

    #[test]
    fn overridden_header_values_do_not_prompt_or_fetch_and_names_expand_once() {
        let mut p = project_with_host("http://unused");
        p.environments[0].values["header"] = json!("X-Key");
        p.environments[0].values["child"] = json!("x-key");
        p.environments[0].values["literal_name"] = json!("{{header}}");
        p.environments[0].values["literal_value"] = json!("{{token}}");
        p.requests.push(Request {
            name: "Unused".into(),
            url: "http://unused".into(),
            ..Request::default()
        });
        p.headers
            .push(KeyValue::new("X-Key", "{{ $response('Unused', '$.token') }}"));
        let folder = Folder {
            headers: vec![KeyValue::new("{{header}}", "{{ $prompt('Unused') }}")],
            ..Folder::default()
        };
        let request = Request {
            url: "{{host}}/".into(),
            headers: vec![
                KeyValue::new("{{child}}", "winner"),
                KeyValue::new("{{literal_name}}", "{{literal_value}}"),
            ],
            ..Request::default()
        };
        let (prepared, stale) = prepare_interactive(
            &p,
            &[&folder],
            &request,
            &Responses::default(),
            |_| true,
            Some(&mut |_| panic!("overridden prompt")),
        )
        .unwrap();
        assert!(stale.is_empty());
        assert!(prepared.missing.is_empty());
        assert_eq!(
            prepared
                .headers
                .iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case("x-key"))
                .count(),
            1
        );
        assert!(prepared.headers.contains(&("x-key".into(), "winner".into())));
        assert!(prepared.headers.contains(&("{{header}}".into(), "{{token}}".into())));
    }

    #[test]
    fn url_templates_resolve_before_fragment_removal() {
        let p = project_with_host("http://localhost/from-env#ignored?old=1");
        let request = Request {
            url: "{{host}}".into(),
            query: vec![KeyValue::new("q", "a#b")],
            ..Request::default()
        };
        assert_eq!(
            prepare(&p, &[], &request).unwrap().url,
            "http://localhost/from-env?q=a%23b"
        );
        let request = Request {
            url: "http://localhost/{{ $prompt('Ticket #', '', '123') }}#ignored".into(),
            query: vec![KeyValue::new("q", "yes")],
            ..Request::default()
        };
        let mut asked = 0;
        let (prepared, _) = prepare_interactive(
            &p,
            &[],
            &request,
            &Responses::default(),
            |_| true,
            Some(&mut |spec| {
                asked += 1;
                assert_eq!(spec.label, "Ticket #");
                Ok(spec.default.clone())
            }),
        )
        .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(prepared.url, "http://localhost/123?q=yes");
        assert!(prepared.missing.is_empty());
    }

    #[test]
    fn body_lengths_replace_supplied_headers_on_the_wire_and_in_the_log() {
        let path = std::env::temp_dir().join(format!("fac-length-{}", new_id()));
        std::fs::write(&path, "猫").unwrap();
        for (method, body, expected) in [
            ("POST", Body::Json("123".into()), "123"),
            ("GET", Body::Json("123".into()), "123"),
            ("PUT", Body::File(path.to_string_lossy().into_owned()), "猫"),
        ] {
            let p = project_with_host(&echo());
            let request = Request {
                method: method.into(),
                url: "{{host}}/".into(),
                headers: vec![KeyValue::new("Content-Length", "0")],
                body,
                ..Request::default()
            };
            let mut prepared = prepare(&p, &[], &request).unwrap();
            prepared.headers.push(("content-length".into(), "999".into()));
            let log = request_log(&prepared).to_ascii_lowercase();
            assert!(!log.contains("content-length: 0") && !log.contains("content-length: 999"));
            if matches!(prepared.payload, Payload::Bytes(_)) {
                assert_eq!(log.matches("content-length:").count(), 1);
                assert!(log.contains("content-length: 3\n"));
            }
            let raw = String::from_utf8(send(prepared).unwrap().body).unwrap();
            let headers = raw.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
            assert_eq!(headers.matches("content-length:").count(), 1);
            assert!(headers.contains("content-length: 3"), "{raw}");
            assert!(raw.ends_with(&format!("\r\n\r\n{expected}")), "{raw}");
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn json_comments_do_not_consume_prompt_arguments() {
        use super::*;
        for (body, expected) in [
            ("{{ $prompt('URL http://example.test', '', '123') }}", "123"),
            (
                "{{ $prompt('Payload', '/* help */', '{\"url\":\"http://example.test\"}') }}",
                "{\"url\":\"http://example.test\"}",
            ),
            (
                r#"{"x":"{{ $prompt('URL //', '', 'a/*b*/c') }}"} // comment"#,
                r#"{"x":"a/*b*/c"} "#,
            ),
            ("// {{ $prompt('ignored') }}\n{{ $prompt('Value', '', '42') }}", "\n42"),
        ] {
            let request = Request {
                body: Body::Json(body.into()),
                ..Request::default()
            };
            let mut asked = 0;
            let (prepared, _) = prepare_interactive(
                &Project::new("P"),
                &[],
                &request,
                &Responses::default(),
                |_| true,
                Some(&mut |spec| {
                    asked += 1;
                    Ok(spec.default.clone())
                }),
            )
            .unwrap();
            assert_eq!(asked, 1, "{body}");
            assert_eq!(prepared.payload, Payload::Bytes(expected.as_bytes().to_vec()), "{body}");
            assert!(prepared.missing.is_empty());
        }
    }

    #[test]
    fn json_prompt_defaults_can_contain_nested_objects() {
        use super::*;
        let project = Project::new("P");
        let request = Request {
            body: Body::Json(r#"{{ $prompt('Payload', '', '{"a":{}}') }}"#.into()),
            ..Request::default()
        };
        let mut asked = 0;
        let (prepared, _) = prepare_interactive(
            &project,
            &[],
            &request,
            &Responses::default(),
            |_| true,
            Some(&mut |spec| {
                asked += 1;
                Ok(spec.default.clone())
            }),
        )
        .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(prepared.payload, Payload::Bytes(br#"{"a":{}}"#.to_vec()));
        assert!(prepared.missing.is_empty());
    }

    #[test]
    fn automatic_text_limit_counts_bytes_and_bounds_formatted_output() {
        use super::{AUTO_TEXT_LIMIT, pretty_json, pretty_xml, small_text};
        for size in [AUTO_TEXT_LIMIT - 1, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT + 1] {
            let json = format!("[0]{}", " ".repeat(size - 3));
            let xml = format!("<r/>{}", " ".repeat(size - 4));
            assert_eq!(pretty_json(&json).is_some(), size <= AUTO_TEXT_LIMIT);
            assert_eq!(pretty_xml(&xml).is_some(), size <= AUTO_TEXT_LIMIT);
        }
        let text = "猫".repeat(AUTO_TEXT_LIMIT / 3);
        assert!(small_text(&text));
        assert!(small_text(&format!("{text}x")));
        assert!(!small_text(&format!("{text}猫")));
        let json = format!("[{}0]", "0,".repeat(16_384));
        assert!(small_text(&json));
        assert!(pretty_json(&json).is_none());
        let xml = format!("{}{}", "<r>".repeat(300), "</r>".repeat(300));
        assert!(small_text(&xml));
        assert!(pretty_xml(&xml).is_none());
    }

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
    fn pretty_json_decodes_unicode_without_changing_string_values() {
        let src = r#"{"success":false,"data":"Ops, n\u00e3o foi poss\u00edvel realizar essa a\u00e7\u00e3o, tente novamente."}"#;
        let want =
            "{\n  \"success\": false,\n  \"data\": \"Ops, não foi possível realizar essa ação, tente novamente.\"\n}";
        assert_eq!(super::pretty_json(src).as_deref(), Some(want));

        let src =
            r#"{"\u00e3":"\uD83D\uDE00","literal":"\\u00e3","safe":"\u0022\u005c\u000a\u0009\u0000","text":"ação"}"#;
        let pretty = super::pretty_json(src).unwrap();
        assert!(pretty.contains("\"ã\": \"😀\""));
        assert!(pretty.contains(r#""literal": "\\u00e3""#));
        assert!(pretty.contains(r#""safe": "\"\\\n\t\u0000""#));
        assert!(pretty.contains("ação"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(src).unwrap(),
            serde_json::from_str::<serde_json::Value>(&pretty).unwrap()
        );
        for invalid in [r#"{"s":"\u00"}"#, r#"{"s":"\uD800"}"#, r#"{"s":"\uZZZZ"}"#] {
            assert_eq!(super::pretty_json(invalid), None);
        }
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
        sync::mpsc,
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

    #[test]
    fn cancel_interrupts_a_blocked_response_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(2));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        });
        let cancel = CancelHandle::default();
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            send_cancellable(
                Prepared {
                    method: "GET".into(),
                    url,
                    headers: vec![],
                    payload: Payload::None,
                    missing: vec![],
                },
                &worker_cancel,
            )
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.cancel();
        assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
        server.join().unwrap();
    }

    #[test]
    fn capture_limit_keeps_exactly_one_mib_and_marks_only_larger_bodies() {
        for size in [MAX_BODY - 1, MAX_BODY, MAX_BODY + 1] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                stream.write_all(&vec![b'x'; size]).unwrap();
            });
            let response = send(Prepared {
                method: "GET".into(),
                url,
                headers: vec![],
                payload: Payload::None,
                missing: vec![],
            })
            .unwrap();
            assert_eq!(response.body, vec![b'x'; size.min(MAX_BODY)]);
            assert_eq!(response.truncated, size > MAX_BODY);
            server.join().unwrap();
        }
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
    fn noninteractive_preparation_keeps_prompts_and_interactive_answers_are_literal() {
        let p = Project::new("Prompts");
        let r = Request {
            url: "{{ $prompt('Host') }}/path".into(),
            headers: vec![KeyValue::new("X-Value", "{{ $prompt('Header') }}")],
            body: Body::Xml("{{ $prompt('Body') }}".into()),
            ..Default::default()
        };
        let plain = prepare(&p, &[], &r).unwrap();
        assert_eq!(plain.url, r.url);
        assert_eq!(plain.missing.len(), 3);
        let mut calls = 0;
        let (prepared, _) = prepare_interactive(
            &p,
            &[],
            &r,
            &Responses::default(),
            |_| false,
            Some(&mut |spec| {
                calls += 1;
                Ok(match spec.label.as_str() {
                    "Host" => "http://localhost",
                    "Header" => "",
                    _ => "{{ $prompt('literal') }}",
                }
                .into())
            }),
        )
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(prepared.url, "http://localhost/path");
        assert_eq!(prepared.headers[0].1, "");
        assert_eq!(prepared.payload, Payload::Bytes(b"{{ $prompt('literal') }}".to_vec()));
        assert!(prepared.missing.is_empty());
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
