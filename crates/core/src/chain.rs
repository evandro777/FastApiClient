//! `{{ $response('Request', '$.access_token', '$.expires_in') }}`: a value from another request's JSON response,
//! fetched on demand and kept in memory until it expires.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    http::{self, Error, Prepared, Response},
    model::{Folder, Project, Request},
    resolve::{Prompt, args, json_path, lookup},
};

/// How many requests may wait on one another before it is taken for a loop.
const MAX_DEPTH: usize = 4;
const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_ENTRIES: usize = 256;

#[derive(Default)]
struct Preparation {
    depth: usize,
    fresh: HashSet<String>,
}

pub enum Lookup {
    Value(String),
    /// Nothing cached for this request, or it has expired: fetch it first.
    Stale(String),
    /// Not a valid call, or the path is not in the cached body.
    Bad,
}

struct ProjectResponses {
    source: Box<[u8]>,
    values: HashMap<String, (Arc<[u8]>, Instant)>,
}

fn cache_usage(cache: &HashMap<String, ProjectResponses>) -> (usize, usize) {
    cache.iter().fold((0, 0), |(bytes, count), (id, project)| {
        (
            bytes
                + id.len()
                + project.source.len()
                + project
                    .values
                    .iter()
                    .map(|(name, (body, _))| name.len() + body.len())
                    .sum::<usize>(),
            count + project.values.len(),
        )
    })
}

/// A project snapshot prevents reuse after environment/definition changes, including late worker results.
#[derive(Default)]
pub struct Responses {
    cache: Arc<Mutex<HashMap<String, ProjectResponses>>>,
    generations: Vec<(Arc<AtomicU64>, u64)>,
}

impl Responses {
    /// Shares cached values, but rejects mutations from this Send after its selection/window is cancelled.
    pub fn for_send(&self, generation: Arc<AtomicU64>, started: u64) -> Self {
        Self {
            cache: self.cache.clone(),
            generations: vec![(generation, started)],
        }
    }

    pub fn for_send_with(&self, generation: Arc<AtomicU64>, started: u64) -> Self {
        let mut send = Self {
            cache: self.cache.clone(),
            generations: self.generations.clone(),
        };
        send.generations.push((generation, started));
        send
    }

    fn check_current(&self) -> Result<(), Error> {
        if self
            .generations
            .iter()
            .any(|(generation, started)| generation.load(Ordering::SeqCst) != *started)
        {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Forget a response before refreshing it, including when sending fails without an HTTP response.
    pub fn invalidate(&self, project: &Project, request: &str) -> Result<(), Error> {
        let source = serde_json::to_vec(project).expect("a project always serializes");
        let mut cache = self.cache.lock().unwrap();
        self.check_current()?;
        if let Some(entry) = cache
            .get_mut(&project.id)
            .filter(|entry| entry.source.as_ref() == source)
        {
            entry.values.remove(request);
            if entry.values.is_empty() {
                cache.remove(&project.id);
            }
        }
        Ok(())
    }

    /// An unsuccessful or incomplete refresh removes the previous value instead of reviving an expired token.
    pub fn store(&self, project: &Project, request: &str, r: &Response) -> bool {
        self.store_checked(project, request, r).unwrap_or(false)
    }

    /// Cancel in-flight Sends before removing a deleted project, so late workers cannot recreate its cache.
    pub fn remove_project(&self, id: &str) {
        self.cache.lock().unwrap().remove(id);
    }

    fn store_checked(&self, project: &Project, request: &str, r: &Response) -> Result<bool, Error> {
        self.check_current()?;
        let source = serde_json::to_vec(project)
            .expect("a project always serializes")
            .into_boxed_slice();
        let valid = (200..300).contains(&r.status)
            && !r.truncated
            && r.body.len() <= http::MAX_BODY
            && serde_json::from_slice::<Value>(&r.body).is_ok();
        let mut cache = self.cache.lock().unwrap();
        // Check under the same lock as publication: an older writer cannot pass a newer one.
        self.check_current()?;
        if cache.get(&project.id).is_none_or(|entry| entry.source != source) {
            cache.insert(
                project.id.clone(),
                ProjectResponses {
                    source,
                    values: HashMap::new(),
                },
            );
        }
        let entry = cache.get_mut(&project.id).unwrap();
        entry.values.remove(request);
        if !valid {
            if entry.values.is_empty() {
                cache.remove(&project.id);
            }
            return Ok(false);
        }
        if project.id.len() + entry.source.len() + request.len() + r.body.len() > CACHE_BYTES {
            if entry.values.is_empty() {
                cache.remove(&project.id);
            }
            return Err(Error::CacheLimit);
        }
        entry
            .values
            .insert(request.into(), (Arc::from(r.body.as_slice()), Instant::now()));
        loop {
            let (bytes, count) = cache_usage(&cache);
            if bytes <= CACHE_BYTES && count <= CACHE_ENTRIES {
                break;
            }
            let (id, name) = cache
                .iter()
                .flat_map(|(id, entry)| entry.values.iter().map(move |(name, (_, at))| (id, name, *at)))
                .min_by_key(|(id, name, at)| (id.as_str() == project.id && name.as_str() == request, *at))
                .map(|(id, name, _)| (id.clone(), name.clone()))
                .expect("cache has a response");
            let entry = cache.get_mut(&id).unwrap();
            entry.values.remove(&name);
            if entry.values.is_empty() {
                cache.remove(&id);
            }
        }
        if cache
            .get(&project.id)
            .is_some_and(|entry| entry.values.contains_key(request))
        {
            Ok(true)
        } else {
            Err(Error::CacheLimit)
        }
    }

    /// Resolves `$response('name', 'path', expiry?)`. `expiry` is seconds, a `$.path` into the same response, or the
    /// name of an environment variable holding the seconds; one that cannot be read counts as expired, so a token is
    /// never reused past a limit that cannot be checked.
    pub fn lookup(&self, project: &Project, expr: &str, check_age: bool) -> Lookup {
        self.lookup_in(
            project,
            &serde_json::to_vec(project).expect("a project always serializes"),
            expr,
            check_age,
        )
    }

    /// `lookup` with the project's serialized form already made: one Send resolves many tokens of one project.
    pub(crate) fn lookup_in(&self, project: &Project, source: &[u8], expr: &str, check_age: bool) -> Lookup {
        let a = args(expr, "$response").unwrap_or_default();
        if !(2..=3).contains(&a.len()) {
            return Lookup::Bad;
        }
        let Some((body, at)) = ({
            let cache = self.cache.lock().unwrap();
            cache
                .get(&project.id)
                .filter(|entry| entry.source.as_ref() == source)
                .and_then(|entry| entry.values.get(&a[0]))
                .cloned()
        }) else {
            return Lookup::Stale(a[0].clone());
        };
        // Parse one bounded response outside the cache lock, rather than retaining expanded trees.
        let Ok(body) = serde_json::from_slice::<Value>(&body) else {
            return Lookup::Bad;
        };
        if check_age && let Some(e) = a.get(2) {
            let seconds = if e.starts_with('$') {
                json_path(&body, e)
            } else if e.parse::<f64>().is_ok() {
                Some(e.clone())
            } else {
                lookup(project.active_values(), e)
            };
            match seconds.and_then(|s| s.trim().parse::<f64>().ok()) {
                Some(s) if at.elapsed() < Duration::try_from_secs_f64(s).unwrap_or_default() => {}
                _ => return Lookup::Stale(a[0].clone()),
            }
        }
        json_path(&body, &a[1]).map_or(Lookup::Bad, Lookup::Value)
    }
}

/// `http::prepare_with` that first sends the requests whose response `request` needs and is missing or expired.
/// Blocking: call it from a thread.
pub fn prepare(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
) -> Result<Prepared, Error> {
    let cancel = http::CancelHandle::default();
    prepare_at(
        project,
        folders,
        request,
        cache,
        &mut Preparation::default(),
        &mut None,
        &mut |_| Ok(()),
        &mut || {},
        &cancel,
    )
}

/// One answer lifetime across both preparation passes and all dependencies. The caller confirms files before send.
pub fn prepare_interactive(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    prompt: &mut dyn FnMut(&Prompt) -> Result<String, Error>,
    before_send: &mut dyn FnMut(&Prepared) -> Result<(), Error>,
) -> Result<Prepared, Error> {
    prepare_interactive_with_progress(project, folders, request, cache, prompt, before_send, &mut || {})
}

/// The progress callback runs before each preparation pass, including dependency requests.
pub fn prepare_interactive_with_progress(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    prompt: &mut dyn FnMut(&Prompt) -> Result<String, Error>,
    before_send: &mut dyn FnMut(&Prepared) -> Result<(), Error>,
    progress: &mut dyn FnMut(),
) -> Result<Prepared, Error> {
    prepare_interactive_with_cancel(
        project,
        folders,
        request,
        cache,
        prompt,
        before_send,
        progress,
        &http::CancelHandle::default(),
    )
}

/// Like `prepare_interactive_with_progress`, while allowing the caller to interrupt dependency requests.
#[allow(clippy::too_many_arguments)]
pub fn prepare_interactive_with_cancel(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    prompt: &mut dyn FnMut(&Prompt) -> Result<String, Error>,
    before_send: &mut dyn FnMut(&Prepared) -> Result<(), Error>,
    progress: &mut dyn FnMut(),
    cancel: &http::CancelHandle,
) -> Result<Prepared, Error> {
    let mut answers = HashMap::new();
    let mut cached = |spec: &Prompt| {
        if let Some(answer) = answers.get(spec) {
            return Ok(String::clone(answer));
        }
        let answer = prompt(spec)?;
        answers.insert(spec.clone(), answer.clone());
        Ok(answer)
    };
    let prepared = prepare_at(
        project,
        folders,
        request,
        cache,
        &mut Preparation::default(),
        &mut Some(&mut cached),
        before_send,
        progress,
        cancel,
    )?;
    before_send(&prepared)?;
    Ok(prepared)
}

#[allow(clippy::too_many_arguments)]
fn prepare_at(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    state: &mut Preparation,
    prompt: &mut Option<&mut http::PromptResolver<'_>>,
    before_send: &mut dyn FnMut(&Prepared) -> Result<(), Error>,
    progress: &mut dyn FnMut(),
    cancel: &http::CancelHandle,
) -> Result<Prepared, Error> {
    loop {
        cache.check_current()?;
        progress();
        let (first, stale) = http::prepare_interactive(
            project,
            folders,
            request,
            cache,
            |name| !state.fresh.contains(name),
            prompt
                .as_mut()
                .map(|f| &mut **f as &mut dyn FnMut(&Prompt) -> Result<String, Error>),
        )?;
        let Some(name) = stale.into_iter().next() else {
            return Ok(first);
        };
        if state.fresh.contains(&name) {
            return Err(Error::CacheLimit);
        }
        if state.depth >= MAX_DEPTH {
            return Err(Error::Cycle(name));
        }
        let Some((parents, dependency)) = project.find_request(&name) else {
            return Ok(first);
        };
        let failed = |e: Error| match e {
            Error::Cycle(_) | Error::Cancelled | Error::DependencyJson(_) | Error::CacheLimit => e,
            e => Error::Dependency(name.clone(), e.to_string()),
        };
        // A failed refresh (including network errors or cancellation) must not leave a reusable old value.
        cache.invalidate(project, &name)?;
        state.depth += 1;
        let prepared = prepare_at(
            project,
            &parents,
            dependency,
            cache,
            state,
            prompt,
            before_send,
            progress,
            cancel,
        );
        state.depth -= 1;
        let response = prepared.and_then(|p| {
            before_send(&p)?;
            http::send_cancellable(p, cancel)
        });
        cache.check_current()?;
        let response = response.map_err(failed)?;
        if !(200..300).contains(&response.status) {
            return Err(Error::Dependency(
                name,
                format!("{} {}", response.status, response.reason),
            ));
        }
        if !cache.store_checked(project, &name, &response)? {
            cache.check_current()?;
            return Err(Error::DependencyJson(name));
        }
        state.fresh.insert(name);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cache_evicts_older_responses_at_both_limits() {
        let project = Project::new("Bounded");
        for (count, body) in [
            (CACHE_ENTRIES + 1, "0".into()),
            (34, format!("\"{}\"", "x".repeat(http::MAX_BODY - 2))),
        ] {
            let cache = Responses::default();
            let response = response(&body);
            for i in 0..count {
                assert!(cache.store(&project, &format!("R{i}"), &response));
            }
            let values = cache.cache.lock().unwrap();
            let (bytes, entries) = cache_usage(&values);
            assert!(bytes <= CACHE_BYTES && entries <= CACHE_ENTRIES);
            assert!(!values[&project.id].values.contains_key("R0"));
            assert!(values[&project.id].values.contains_key(&format!("R{}", count - 1)));
            drop(values);
            cache.remove_project(&project.id);
            assert!(cache.cache.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn cache_retains_json_bytes_instead_of_expanded_trees() {
        let project = Project::new("Compact");
        let cache = Responses::default();
        let body = format!("[{}0]", "0,".repeat(100_000));
        for i in 0..8 {
            assert!(cache.store(&project, &format!("R{i}"), &response(&body)));
        }
        assert!(cache_usage(&cache.cache.lock().unwrap()).0 < body.len() * 9);
        assert!(
            matches!(cache.lookup(&project, "$response('R0', '$[100000]')", true), Lookup::Value(value) if value == "0")
        );
    }

    #[test]
    fn an_evicted_send_dependency_fails_instead_of_fetching_forever() {
        let (host, hits) = server("200 OK", r#"{"access_token":"token"}"#);
        let mut project = project(&host, "");
        project.requests.push(Request {
            name: "Second".into(),
            url: format!("{host}/second"),
            ..Request::default()
        });
        project.requests[1].body = crate::model::Body::Xml("{{ $response('Second', '$.access_token') }}".into());
        let cache = Responses::default();
        let result = prepare_interactive(
            &project,
            &[],
            &project.requests[1],
            &cache,
            &mut |_| panic!("no prompt"),
            &mut |prepared| {
                if prepared.url.ends_with("/second") {
                    cache.remove_project(&project.id);
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::CacheLimit)));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn header_name_dependencies_resolve_before_overridden_values_and_auth() {
        use crate::model::{Body, KeyValue, OAuth2};
        let (names, name_hits) = server("200 OK", r#"{"key":"X-Key","token":"X-Token","auth":"Authorization"}"#);
        let (body, body_hits) = server("200 OK", r#"{"value":"fresh"}"#);
        let mut p = Project::new("Names");
        p.auth = Auth::OAuth2(OAuth2::default());
        p.requests = vec![
            Request {
                name: "Names".into(),
                url: names,
                auth: Auth::None,
                ..Request::default()
            },
            Request {
                name: "Body".into(),
                url: body,
                auth: Auth::None,
                ..Request::default()
            },
            Request {
                name: "Unused".into(),
                url: "http://unused".into(),
                ..Request::default()
            },
        ];
        let folder = Folder {
            headers: vec![
                KeyValue::new("X-Key", "{{ $prompt('Unused') }}"),
                KeyValue::new("X-Token", "{{ $response('Unused', '$.value') }}"),
            ],
            requests: vec![Request {
                name: "Main".into(),
                url: "http://unused".into(),
                headers: vec![
                    KeyValue::new("{{ $response('Names', '$.key', 0) }}", "winner"),
                    KeyValue::new("{{ $response('Names', '$.token', 0) }}", "winner-token"),
                    KeyValue::new("{{ $response('Names', '$.auth', 0) }}", "Bearer winner"),
                ],
                body: Body::Xml("{{ $response('Body', '$.value', 0) }}".into()),
                ..Request::default()
            }],
            ..Folder::default()
        };
        p.folders.push(folder);
        let cache = Responses::default();
        cache.store(&p, "Body", &response(r#"{"value":"old"}"#));
        let prepared = prepare_interactive(
            &p,
            &[&p.folders[0]],
            &p.folders[0].requests[0],
            &cache,
            &mut |_| panic!("overridden prompt"),
            &mut |_| Ok(()),
        )
        .unwrap();
        assert!(prepared.missing.is_empty(), "{:?}", prepared.missing);
        assert!(prepared.headers.contains(&("X-Key".into(), "winner".into())));
        assert!(prepared.headers.contains(&("X-Token".into(), "winner-token".into())));
        assert!(
            prepared
                .headers
                .contains(&("Authorization".into(), "Bearer winner".into()))
        );
        assert_eq!(prepared.payload, http::Payload::Bytes(b"fresh".to_vec()));
        assert_eq!(name_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            body_hits.load(Ordering::SeqCst),
            1,
            "newly discovered expired dependencies must refresh"
        );
    }

    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serde_json::json;

    use super::*;
    use crate::model::{Auth, Environment};

    fn response(body: &str) -> Response {
        Response {
            status: 200,
            reason: "OK".into(),
            headers: vec![],
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn response_numbers_keep_their_digits_when_used_in_requests() {
        let p = project("http://unused", "");
        let cache = Responses::default();
        for number in [
            "12345678901234567890123",
            "-12345678901234567890123",
            "0.12345678901234567890123",
        ] {
            assert!(cache.store(&p, "Token", &response(&format!("{{\"id\":{number}}}"))));
            let request = Request {
                url: "http://unused/items/{{ $response('Token', '$.id') }}".into(),
                body: crate::model::Body::Json("{\"id\":{{ $response('Token', '$.id') }}}".into()),
                ..Request::default()
            };
            let prepared = prepare(&p, &[], &request, &cache).unwrap();
            assert_eq!(prepared.url, format!("http://unused/items/{number}"));
            assert_eq!(
                prepared.payload,
                http::Payload::Bytes(format!("{{\"id\":{number}}}").into_bytes())
            );
            assert!(prepared.missing.is_empty());
        }
    }

    #[test]
    fn cancelled_sends_cannot_replace_or_clear_newer_cached_values() {
        let p = project("http://unused", ", 0");
        let cache = Responses::default();
        let generation = Arc::new(AtomicU64::new(0));
        let old = cache.for_send(generation.clone(), 0);
        generation.store(1, Ordering::SeqCst);
        let current = cache.for_send(generation, 1);
        current.store(&p, "Token", &response(r#"{"access_token":"new"}"#));
        let at = cache.cache.lock().unwrap()[&p.id].values["Token"].1;
        let mut failed = response("{}");
        failed.status = 401;
        let mut truncated = response("{}");
        truncated.truncated = true;
        for late in [
            response(r#"{"access_token":"old"}"#),
            response("not JSON"),
            failed,
            truncated,
        ] {
            assert!(!old.store(&p, "Token", &late));
        }
        assert!(matches!(run(&p, "Field", &old), Err(Error::Cancelled)));
        assert!(matches!(old.invalidate(&p, "Token"), Err(Error::Cancelled)));
        assert!(
            matches!(cache.lookup(&p, "$response('Token', '$.access_token')", true), Lookup::Value(v) if v == "new")
        );
        assert_eq!(cache.cache.lock().unwrap()[&p.id].values["Token"].1, at);
    }

    #[test]
    fn cancelled_dependency_completion_keeps_the_newer_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = project(&format!("http://{}", listener.local_addr().unwrap()), "");
        let (accepted, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            accepted.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(10)).unwrap();
            let body = r#"{"access_token":"old"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let cache = Responses::default();
        let generation = Arc::new(AtomicU64::new(0));
        let old = cache.for_send(generation.clone(), 0);
        let worker_project = p.clone();
        let worker = std::thread::spawn(move || {
            prepare_interactive(
                &worker_project,
                &[],
                &worker_project.requests[1],
                &old,
                &mut |_| panic!("no prompt"),
                &mut |_| Ok(()),
            )
        });
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        generation.store(1, Ordering::SeqCst);
        cache
            .for_send(generation, 1)
            .store(&p, "Token", &response(r#"{"access_token":"new"}"#));
        release.send(()).unwrap();
        assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
        server.join().unwrap();
        assert!(
            matches!(cache.lookup(&p, "$response('Token', '$.access_token')", true), Lookup::Value(v) if v == "new")
        );
    }

    #[test]
    fn cache_rejects_changed_contexts_and_late_results() {
        let original = project("http://unused", "");
        let mut environment = original.clone();
        environment.active_env = "other".into();
        let mut values = original.clone();
        values.environments[0].values["host"] = json!("http://other");
        let mut definition = original.clone();
        definition.requests[0].url = "http://changed".into();
        let mut auth = original.clone();
        auth.auth = Auth::Bearer("changed".into());
        let cache = Responses::default();
        let expr = "$response('Token', '$.access_token')";
        for changed in [environment, values, definition, auth] {
            cache.store(&original, "Token", &response(r#"{"access_token":"old"}"#));
            assert!(matches!(cache.lookup(&changed, expr, true), Lookup::Stale(_)));
            cache.store(&changed, "Token", &response(r#"{"access_token":"new"}"#));
            assert!(matches!(cache.lookup(&changed, expr, true), Lookup::Value(v) if v == "new"));
            // A worker started before the change finishes after the newer worker.
            cache.store(&original, "Token", &response(r#"{"access_token":"late"}"#));
            assert!(matches!(cache.lookup(&changed, expr, true), Lookup::Stale(_)));
        }
    }

    #[test]
    fn unusable_refresh_cannot_revive_an_expired_token() {
        let (host, hits) = server("200 OK", "<html>Login required</html>");
        let p = project(&host, ", 0");
        let cache = Responses::default();
        cache.store(&p, "Token", &response(r#"{"access_token":"expired"}"#));
        assert!(matches!(run(&p, "Field", &cache), Err(Error::DependencyJson(name)) if name == "Token"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(matches!(
            cache.lookup(&p, "$response('Token', '$.access_token')", false),
            Lookup::Stale(_)
        ));
        let mut truncated = response(r#"{"access_token":"cut"}"#);
        truncated.truncated = true;
        assert!(!cache.store(&p, "Token", &truncated));
    }

    #[test]
    fn http_failure_also_forgets_expired_values() {
        let (host, _) = server("401 Unauthorized", "{}");
        let p = project(&host, ", 0");
        let cache = Responses::default();
        cache.store(&p, "Token", &response(r#"{"access_token":"expired"}"#));
        assert!(matches!(run(&p, "Field", &cache), Err(Error::Dependency(_, _))));
        assert!(matches!(
            cache.lookup(&p, "$response('Token', '$.access_token')", false),
            Lookup::Stale(_)
        ));
    }

    #[test]
    fn prompt_answers_are_literal_shared_with_dependencies_and_forgotten_after_send() {
        let (url, hits) = server("200 OK", r#"{"access_token":"token"}"#);
        let mut p = project(&url, "access_token");
        let call = "{{ $prompt('Value', 'Description', 'Default') }}";
        p.requests[0]
            .headers
            .push(crate::model::KeyValue::new("X-Prompt", call));
        p.requests[1]
            .headers
            .push(crate::model::KeyValue::new("X-Prompt", call));
        p.requests[1].body = crate::model::Body::Xml(call.into());
        let original = serde_json::to_string(&p).unwrap();
        let cache = Responses::default();
        let mut asked = 0;
        let mut confirmed = 0;
        for answer in ["{{host}} $date", ""] {
            let prepared = prepare_interactive(
                &p,
                &[],
                &p.requests[1],
                &cache,
                &mut |spec| {
                    asked += 1;
                    assert_eq!(
                        (spec.label.as_str(), spec.description.as_str(), spec.default.as_str()),
                        ("Value", "Description", "Default")
                    );
                    Ok(answer.into())
                },
                &mut |_| {
                    confirmed += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(
                prepared.headers.iter().find(|(k, _)| k == "X-Prompt").unwrap().1,
                answer
            );
            assert_eq!(prepared.payload, http::Payload::Bytes(answer.as_bytes().to_vec()));
        }
        assert_eq!(
            asked, 2,
            "one prompt per Send, including both preparation passes and dependencies"
        );
        assert_eq!(confirmed, 3, "dependency and main go through confirmation");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(serde_json::to_string(&p).unwrap(), original);
    }

    #[test]
    fn prompt_cancel_stops_dependencies_and_is_not_wrapped_as_failure() {
        let mut p = project("http://localhost:9", "access_token");
        p.requests[0].url = "{{ $prompt('Host') }}".into();
        let mut sends = 0;
        let result = prepare_interactive(
            &p,
            &[],
            &p.requests[1],
            &Responses::default(),
            &mut |_| Err(Error::Cancelled),
            &mut |_| {
                sends += 1;
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(sends, 0);
    }

    /// Answers every request with `status` and `body`; returns the base URL and a hit counter.
    fn server(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, hits)
    }

    fn project(host: &str, token_field: &str) -> Project {
        let call = |expiry: &str| format!("{{{{ $response('Token', '$.access_token'{expiry}) }}}}");
        let main = |name: &str, expiry: &str| Request {
            name: name.into(),
            auth: Auth::Bearer(call(expiry)),
            ..Request::default()
        };
        Project {
            id: "p".into(),
            environments: vec![Environment {
                name: "e".into(),
                values: json!({ "host": host }),
            }],
            active_env: "e".into(),
            requests: vec![
                Request {
                    name: "Token".into(),
                    url: "{{host}}/token".into(),
                    ..Request::default()
                },
                main("Once", ""),
                main("Hour", ", 3600"),
                main("Field", token_field),
            ],
            ..Project::default()
        }
    }

    fn bearer(p: &Prepared) -> Option<&str> {
        p.headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .map(|(_, v)| v.as_str())
    }

    fn run(p: &Project, name: &str, cache: &Responses) -> Result<Prepared, Error> {
        let (_, r) = p.find_request(name).unwrap();
        prepare(p, &[], r, cache)
    }

    #[test]
    fn token_is_fetched_once_and_reused() {
        let (host, hits) = server("200 OK", r#"{"access_token":"abc","expires_in":3600}"#);
        let (p, cache) = (project(&host, ""), Responses::default());
        for name in ["Once", "Hour", "Once"] {
            assert_eq!(bearer(&run(&p, name, &cache).unwrap()), Some("Bearer abc"));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn progress_runs_for_dependency_and_final_preparation() {
        let (host, _) = server("200 OK", r#"{"access_token":"abc","expires_in":3600}"#);
        let (project, cache) = (project(&host, ""), Responses::default());
        let mut passes = 0;
        prepare_interactive_with_progress(
            &project,
            &[],
            project.find_request("Once").unwrap().1,
            &cache,
            &mut |_| panic!("no prompt expected"),
            &mut |_| Ok(()),
            &mut || passes += 1,
        )
        .unwrap();
        assert_eq!(passes, 3);
    }

    #[test]
    fn expired_token_is_fetched_again() {
        let (host, hits) = server("200 OK", r#"{"access_token":"abc","expires_in":0}"#);
        let (p, cache) = (project(&host, ", '$.expires_in'"), Responses::default());
        for _ in 0..2 {
            assert_eq!(bearer(&run(&p, "Field", &cache).unwrap()), Some("Bearer abc"));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2, "an expiry of 0 is always stale");
    }

    #[test]
    fn expiry_can_be_an_environment_variable() {
        let (host, hits) = server("200 OK", r#"{"access_token":"abc"}"#);
        let (mut p, cache) = (project(&host, ", 'ttl'"), Responses::default());
        p.environments[0].values["ttl"] = json!(3600);
        run(&p, "Field", &cache).unwrap();
        run(&p, "Field", &cache).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        p.environments[0].values["ttl"] = json!(0);
        run(&p, "Field", &cache).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_missing_expiry_field_means_expired() {
        let (host, hits) = server("200 OK", r#"{"access_token":"abc"}"#);
        let (p, cache) = (project(&host, ", '$.expires_in'"), Responses::default());
        run(&p, "Field", &cache).unwrap();
        run(&p, "Field", &cache).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failing_dependency_stops_the_request() {
        let (host, _) = server("401 Unauthorized", "{}");
        let (p, cache) = (project(&host, ""), Responses::default());
        let e = run(&p, "Once", &cache).unwrap_err();
        assert!(
            matches!(&e, Error::Dependency(n, d) if n == "Token" && d == "401 Unauthorized"),
            "{e}"
        );
    }

    #[test]
    fn requests_that_need_themselves_are_cut() {
        let mut p = project("http://unused", "");
        p.requests[0].auth = Auth::Bearer("{{ $response('Token', '$.access_token') }}".into());
        let e = run(&p, "Token", &Responses::default()).unwrap_err();
        assert!(matches!(e, Error::Cycle(n) if n == "Token"));
    }

    #[test]
    fn unknown_request_or_path_is_undefined_not_fetched() {
        let (host, hits) = server("200 OK", r#"{"access_token":"abc"}"#);
        let mut p = project(&host, "");
        p.requests[1].auth = Auth::Bearer("{{ $response('Nope', '$.x') }}".into());
        let r = run(&p, "Once", &Responses::default()).unwrap();
        assert_eq!(r.missing, ["$response('Nope', '$.x')"]);
        p.requests[1].auth = Auth::Bearer("{{ $response('Token', '$.nope') }}".into());
        let r = run(&p, "Once", &Responses::default()).unwrap();
        assert_eq!(r.missing, ["$response('Token', '$.nope')"]);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "only the real request is fetched");
    }
}
