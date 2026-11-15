//! Canned data for `scripts/ui-snapshot.sh` (`FASTAPICLIENT_FIXTURES=empty|single|many|error`): every page can be
//! rendered with zero, one or many projects, rows and a response, without touching the disk or the network.
//! `FASTAPICLIENT_SELECT=project|folder|other` opens a project, a folder or the second project instead of a request.
use fastapiclient_core::{
    model::{Auth, Basic, Body, Environment, Folder, KeyValue, Project, Request},
    workspace::{Sel, Workspace},
};

use crate::Main;

/// 0 empty, 1 single, 2 many, 3 error; `None` when the variable is unset or unknown.
pub fn mode() -> Option<usize> {
    match std::env::var("FASTAPICLIENT_FIXTURES").ok()?.as_str() {
        "empty" => Some(0),
        "single" => Some(1),
        "many" => Some(2),
        "error" => Some(3),
        _ => None,
    }
}

fn kv(i: usize, key: &str) -> KeyValue {
    match i {
        // Long texts to check truncation, and one disabled row to check the checkbox.
        1 => KeyValue {
            key: format!("{key}-with-a-very-long-name-that-should-be-cut-off"),
            value: "a value that is also far too long to fit in its column on one line of the table".into(),
            description: "a long description that should be cut off instead of stretching the row".into(),
            enabled: true,
        },
        2 => KeyValue {
            enabled: false,
            ..KeyValue::new(&format!("{key}3"), "off")
        },
        _ => KeyValue {
            description: if i.is_multiple_of(2) {
                "what it does".into()
            } else {
                String::new()
            },
            ..KeyValue::new(&format!("{key}{}", i + 1), &format!("value {}", i + 1))
        },
    }
}

fn kvs(n: usize, key: &str) -> Vec<KeyValue> {
    (0..n).map(|i| kv(i, key)).collect()
}

fn request(name: &str, method: &str, n: usize) -> Request {
    Request {
        name: name.into(),
        method: method.into(),
        url: "https://api.example.com/users".into(),
        query: kvs(n, "param"),
        headers: kvs(n, "Header-"),
        ..Request::default()
    }
}

/// The projects of each mode; the first request found is the one that gets opened.
fn projects(mode: usize) -> Vec<Project> {
    let mut one = Project::new("Sample API");
    match mode {
        0 => return vec![],
        1 => {
            one.environments = vec![Environment {
                name: "Local".into(),
                values: serde_json::json!({ "host": "http://localhost:8080", "token": "dev-token" }),
            }];
            one.active_env = "Local".into();
            one.headers = kvs(1, "Accept-");
            one.requests.push(Request {
                body: Body::Json(
                    "{\n  // jsonc is accepted\n  \"name\": \"Ana\",\n  \"tags\": [\"a\", \"b\"]\n}".into(),
                ),
                auth: Auth::Bearer("secret-token".into()),
                ..request("List users", "GET", 1)
            });
        }
        3 => {
            one.requests.push(Request {
                url: "http://localhost:9".into(),
                body: Body::Xml("<user><name>Ana</name></user>".into()),
                auth: Auth::None,
                ..request("Unreachable", "POST", 1)
            });
        }
        _ => {
            one.name = "Payments".into();
            // The second one is active; the last has a long name and a long, nested value to check the editor.
            one.environments = ["Local", "HML", "Production with a very long environment name"]
                .iter()
                .map(|name| Environment {
                    name: (*name).into(),
                    values: serde_json::json!({
                        "host": format!("https://{}.example.com", name.split(' ').next().unwrap_or("").to_lowercase()),
                        "db": { "port": 5432, "options": ["a", "b"] },
                        "token": "a-long-token-value-that-should-scroll-instead-of-stretching-the-editor-sideways-0123456789",
                    }),
                })
                .collect();
            one.active_env = "HML".into();
            one.headers = kvs(12, "Header-");
            one.auth = Auth::Basic(Basic {
                username: "ana".into(),
                password: "hunter2".into(),
            });
            let mut customers = Folder {
                name: "Customers".into(),
                headers: kvs(3, "X-Customer-"),
                ..Folder::default()
            };
            for (i, m) in [
                "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "PURGE", "GET", "GET", "GET", "GET",
            ]
            .iter()
            .enumerate()
            {
                let name = if i == 1 {
                    "A very long request name that should be cut off instead of stretching the sidebar".to_string()
                } else {
                    format!("Request {}", i + 1)
                };
                customers.requests.push(Request {
                    url: "https://api.example.com/v1/organizations/1234567890/projects/abcdefghij/environments/production/requests?verbose=true".into(),
                    body: Body::FormData(kvs(12, "field")),
                    auth: Auth::Basic(Basic {
                        username: "ana".into(),
                        password: "hunter2".into(),
                    }),
                    ..request(&name, m, 12)
                });
            }
            one.folders.push(customers);
            one.folders.push(Folder {
                name: "Invoices".into(),
                requests: vec![request("Create invoice", "POST", 1)],
                ..Folder::default()
            });
            // Two levels down, to see the indentation of nested folders.
            one.folders.push(Folder {
                name: "Archived customers".into(),
                parent: Some(0),
                requests: vec![request("Restore customer", "POST", 1)],
                ..Folder::default()
            });
            one.folders.push(Folder {
                name: "A folder three levels deep with a long name".into(),
                parent: Some(2),
                requests: vec![request("Purge", "DELETE", 0)],
                ..Folder::default()
            });
            one.requests.push(request("Ping", "GET", 0));
            let mut other = Project::new("A very long project name that should be cut off in the sidebar");
            other.requests.push(request("Only request", "DELETE", 1));
            return vec![one, other];
        }
    }
    vec![one]
}

pub fn workspace(mode: usize) -> Workspace {
    let mut ws = Workspace::new(projects(mode));
    let first_request = ws.projects.first().map(|p| {
        // Many: the first request of the first folder; otherwise the first one directly under the project.
        let folder = (!p.folders.is_empty()).then_some(0);
        Sel {
            project: 0,
            folder,
            request: Some(0),
        }
    });
    match std::env::var("FASTAPICLIENT_SELECT").as_deref() {
        Ok("project") => {}
        // The second project (long name), to see that the tree only shows the active one.
        Ok("other") => {
            ws.select_project(1);
        }
        Ok("folder") => {
            ws.select(Sel {
                project: 0,
                folder: Some(0),
                request: None,
            });
        }
        _ => {
            if let Some(s) = first_request {
                ws.select(s);
            }
        }
    }
    ws
}

/// The export or import dialog after an attempt: a success, a long path, or a failure, depending on the fixture mode.
/// `dialog`: 0 export, 1 import, 2 import with the Insomnia format.
pub fn transfer(ui: &Main, mode: usize, dialog: i32) {
    let (kind, detail) = match (mode, dialog) {
        (1, 0) => (1, "/home/ana/Sample_API.fastapiclient.json"),
        (1, _) => (2, "Sample API"),
        (2, 0) => (
            13,
            "/home/ana/documents/work/projects/payments/exports/2026/a-very-long-folder-name",
        ),
        (2, 1) => (12, "3"),
        (2, _) => (10, ""),
        (3, 0) => (14, "Payments_with_a_very_long_project_name.fastapiclient.json"),
        (3, 1) => (8, "expected value at line 1 column 1"),
        (3, _) => (11, "mapping values are not allowed in this context at line 3 column 7"),
        _ => (-1, ""),
    };
    ui.set_transfer_count(3);
    ui.set_transfer_kind(kind);
    ui.set_transfer_detail(detail.into());
}

/// The environment editor with a half-typed value, to see the validation message.
pub fn env_error(ui: &Main) {
    ui.set_env_text("{\n  \"host\": ".into());
    ui.set_env_dirty(true);
    ui.set_env_error_kind(1);
    ui.set_env_error_detail("EOF while parsing a value at line 2 column 10".into());
}

const SAMPLE_LOG: &str = "> POST https://api.shop.com/users?id=1\n> Authorization: Bearer t0k\n> Content-Type: application/json\n> Content-Length: 15\n\n{\"name\":\"Ana\"}";

/// The response panel is not part of the model, so it is filled on its own.
pub fn response(ui: &Main, mode: usize) {
    match mode {
        1 => {
            ui.set_has_response(true);
            ui.set_status(200);
            ui.set_reason("OK".into());
            ui.set_time_ms(87);
            ui.set_size_bytes(143);
            let compact = r#"{"id":1,"name":"Ana","tags":["a","b"],"address":{"city":"Recife","zip":null}}"#;
            ui.set_response_body(compact.into());
            ui.set_response_pretty(
                fastapiclient_core::http::pretty_json(compact)
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_response_beautify(true);
            ui.set_search_text("a".into());
            ui.set_search_open(true);
            ui.set_request_log(SAMPLE_LOG.into());
            ui.set_response_headers("content-type: application/json\ncontent-length: 143".into());
        }
        2 => {
            ui.set_has_response(true);
            ui.set_status(404);
            ui.set_reason("Not Found".into());
            ui.set_time_ms(1234);
            ui.set_size_bytes(1_048_576);
            ui.set_truncated(true);
            ui.set_warning_vars("host, token".into());
            ui.set_request_log(SAMPLE_LOG.into());
            let lines: Vec<String> = (1..=80)
                .map(|n| format!("line {n}: not found, and a long tail to check wrapping of text that does not fit"))
                .collect();
            ui.set_response_body(lines.join("\n").into());
            let headers: Vec<String> = (1..=15).map(|n| format!("x-header-{n}: value {n}")).collect();
            ui.set_response_headers(headers.join("\n").into());
        }
        3 => {
            ui.set_error_kind(5);
            ui.set_error_detail("Connection refused (os error 111)".into());
            ui.set_request_log(SAMPLE_LOG.into());
        }
        _ => {}
    }
}
