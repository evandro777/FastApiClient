//! The responses the viewer shows again when you come back to a request, and the Sends still in flight, per request.
//! Bounded by the same limits as the core's `$response` cache (`chain::CACHE_BYTES` / `CACHE_ENTRIES`).
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use fastapiclient_core::{
    chain::{CACHE_BYTES, CACHE_ENTRIES},
    http::{self, Response},
};

#[derive(Default)]
pub struct SendState {
    pub responses: VecDeque<CachedResponse>,
    pub sends: HashMap<RequestKey, ActiveSend>,
    pub next_send: u64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct RequestKey {
    pub project: String,
    pub folder: Option<usize>,
    pub request: usize,
}

pub struct CachedResponse {
    pub key: RequestKey,
    pub response: Arc<Response>,
    pub request_log: String,
    pub warning_vars: String,
}

pub struct ActiveSend {
    pub serial: u64,
    pub started: Instant,
    pub phase: i32,
    pub method: String,
    pub host: String,
    pub request_log: String,
    pub warning_vars: String,
    pub context: Arc<AtomicU64>,
    pub cancel: http::CancelHandle,
}

pub fn remove_response_context(state: &mut SendState, keys: &HashSet<RequestKey>) {
    state.responses.retain(|saved| !keys.contains(&saved.key));
    for key in keys {
        if let Some(send) = state.sends.remove(key) {
            send.context.fetch_add(1, Ordering::SeqCst);
            send.cancel.cancel();
        }
    }
}

pub fn invalidate_project_context(state: &mut SendState, project: &str) {
    let keys: HashSet<_> = state
        .responses
        .iter()
        .map(|saved| saved.key.clone())
        .chain(state.sends.keys().cloned())
        .filter(|key| key.project == project)
        .collect();
    remove_response_context(state, &keys);
}

pub fn cache_response(
    state: &mut SendState,
    key: RequestKey,
    response: Response,
    request_log: String,
    warning_vars: String,
) {
    state.responses.retain(|saved| saved.key != key);
    state.responses.push_back(CachedResponse {
        key,
        response: Arc::new(response),
        request_log,
        warning_vars,
    });
    while state.responses.len() > CACHE_ENTRIES
        || state
            .responses
            .iter()
            .map(|saved| saved.response.body.len() + saved.request_log.len() + saved.warning_vars.len())
            .sum::<usize>()
            > CACHE_BYTES
    {
        state.responses.pop_front();
    }
}

pub fn take_cached_response(state: &mut SendState, key: &RequestKey) -> Option<(Arc<Response>, String, String)> {
    let index = state.responses.iter().position(|saved| &saved.key == key)?;
    let saved = state.responses.remove(index)?;
    let result = (
        saved.response.clone(),
        saved.request_log.clone(),
        saved.warning_vars.clone(),
    );
    state.responses.push_back(saved);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn response(body: &str) -> Response {
        Response {
            status: 200,
            reason: "OK".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::from_millis(12),
        }
    }

    #[test]
    fn response_history_is_per_request_and_keeps_recent_entries() {
        let mut state = SendState::default();
        let a = RequestKey {
            project: "p".into(),
            folder: None,
            request: 0,
        };
        let b = RequestKey {
            request: 1,
            ..a.clone()
        };
        cache_response(&mut state, a.clone(), response("A"), "GET /a".into(), String::new());
        cache_response(&mut state, b.clone(), response("B"), "GET /b".into(), String::new());
        assert_eq!(take_cached_response(&mut state, &a).unwrap().0.body, b"A");
        assert_eq!(take_cached_response(&mut state, &b).unwrap().0.body, b"B");

        for request in 2..=CACHE_ENTRIES {
            cache_response(
                &mut state,
                RequestKey { request, ..a.clone() },
                response("x"),
                String::new(),
                String::new(),
            );
        }
        assert!(take_cached_response(&mut state, &a).is_none());
        assert!(take_cached_response(&mut state, &b).is_some());
        assert_eq!(state.responses.len(), CACHE_ENTRIES);
    }

    #[test]
    fn invalidating_one_request_drops_its_response_and_cancels_its_send() {
        let mut state = SendState::default();
        let a = RequestKey {
            project: "p".into(),
            folder: Some(0),
            request: 0,
        };
        let b = RequestKey {
            request: 1,
            ..a.clone()
        };
        cache_response(&mut state, a.clone(), response("A"), String::new(), String::new());
        cache_response(&mut state, b.clone(), response("B"), String::new(), String::new());
        let context = Arc::new(AtomicU64::new(0));
        state.sends.insert(
            a.clone(),
            ActiveSend {
                serial: 1,
                started: Instant::now(),
                phase: 1,
                method: String::new(),
                host: String::new(),
                request_log: String::new(),
                warning_vars: String::new(),
                context: context.clone(),
                cancel: http::CancelHandle::default(),
            },
        );
        let removed = HashSet::from([a.clone()]);
        remove_response_context(&mut state, &removed);
        assert_eq!(context.load(Ordering::SeqCst), 1);
        assert!(take_cached_response(&mut state, &a).is_none());
        assert!(take_cached_response(&mut state, &b).is_some());
    }
}
