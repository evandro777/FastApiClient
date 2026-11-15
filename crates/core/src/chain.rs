//! `{{ $response('Request', '$.access_token', '$.expires_in') }}`: a value from another request's JSON response,
//! fetched on demand and kept in memory until it expires.
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    http::{self, Error, Prepared, Response},
    model::{Folder, Project, Request},
    resolve::{args, json_path, lookup},
};

/// How many requests may wait on one another before it is taken for a loop.
const MAX_DEPTH: usize = 4;

pub enum Lookup {
    Value(String),
    /// Nothing cached for this request, or it has expired: fetch it first.
    Stale(String),
    /// Not a valid call, or the path is not in the cached body.
    Bad,
}

/// The last successful JSON response of each request, by (project id, request name).
#[derive(Default)]
pub struct Responses(Mutex<HashMap<(String, String), (Value, Instant)>>);

impl Responses {
    /// Remembers a 2xx JSON response; anything else leaves the previous one alone.
    pub fn store(&self, project: &str, request: &str, r: &Response) {
        if !(200..300).contains(&r.status) {
            return;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&r.body) {
            self.0
                .lock()
                .unwrap()
                .insert((project.into(), request.into()), (v, Instant::now()));
        }
    }

    /// Resolves `$response('name', 'path', expiry?)`. `expiry` is seconds, a `$.path` into the same response, or the
    /// name of an environment variable holding the seconds; one that cannot be read counts as expired, so a token is
    /// never reused past a limit that cannot be checked.
    pub fn lookup(&self, project: &str, env: &Value, expr: &str, check_age: bool) -> Lookup {
        let a = args(expr, "$response").unwrap_or_default();
        if !(2..=3).contains(&a.len()) {
            return Lookup::Bad;
        }
        let cache = self.0.lock().unwrap();
        let Some((body, at)) = cache.get(&(project.to_string(), a[0].clone())) else {
            return Lookup::Stale(a[0].clone());
        };
        if check_age && let Some(e) = a.get(2) {
            let seconds = if e.starts_with('$') {
                json_path(body, e)
            } else if e.parse::<f64>().is_ok() {
                Some(e.clone())
            } else {
                lookup(env, e)
            };
            match seconds.and_then(|s| s.trim().parse::<f64>().ok()) {
                Some(s) if at.elapsed() < Duration::try_from_secs_f64(s).unwrap_or_default() => {}
                _ => return Lookup::Stale(a[0].clone()),
            }
        }
        json_path(body, &a[1]).map_or(Lookup::Bad, Lookup::Value)
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
    prepare_at(project, folders, request, cache, 0)
}

fn prepare_at(
    project: &Project,
    folders: &[&Folder],
    request: &Request,
    cache: &Responses,
    depth: usize,
) -> Result<Prepared, Error> {
    let (first, stale) = http::prepare_with(project, folders, request, cache, true)?;
    if stale.is_empty() {
        return Ok(first);
    }
    for name in stale {
        if depth >= MAX_DEPTH {
            return Err(Error::Cycle(name));
        }
        let Some((parents, dependency)) = project.find_request(&name) else {
            continue;
        };
        let failed = |e: Error| match e {
            Error::Cycle(_) => e,
            e => Error::Dependency(name.clone(), e.to_string()),
        };
        let response = prepare_at(project, &parents, dependency, cache, depth + 1)
            .and_then(http::send)
            .map_err(failed)?;
        if !(200..300).contains(&response.status) {
            return Err(Error::Dependency(
                name,
                format!("{} {}", response.status, response.reason),
            ));
        }
        cache.store(&project.id, &name, &response);
    }
    // Everything is cached now; a value just fetched is used even if its own expiry says otherwise.
    http::prepare_with(project, folders, request, cache, false).map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
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
