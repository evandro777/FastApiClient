//! Export a project to a file and import it back. The file is the project wrapped in a small envelope, so a file of
//! some other kind is recognised and a newer format is refused instead of being half-read.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Auth, Body, KeyValue, Project, new_id};

const FORMAT: &str = "fastapiclient-project";
/// Version 1 holds one project (`project`); version 2 holds all of them (`projects`). A single export stays version
/// 1, so builds that only know that one still read it; they refuse a version 2 file instead of half-reading it.
const SINGLE: u32 = 1;
const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<Project>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    projects: Vec<Project>,
}

#[derive(Debug, PartialEq)]
pub enum ImportError {
    /// The JSON parser's message.
    Invalid(String),
    /// Valid JSON, but not a file this app wrote.
    NotAProject,
    /// Written by a newer version; carries its format version.
    TooNew(u32),
    /// Project headers cannot represent different GLOBAL_HEADERS for each imported environment.
    EnvironmentHeaders,
}

/// Keeps the keys and the shape, drops every value: `{"host": "x", "db": {"port": 1}}` becomes
/// `{"host": "", "db": {"port": ""}}`.
fn blank(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), blank(v))).collect()),
        Value::Array(_) => Value::Array(vec![]),
        _ => Value::String(String::new()),
    }
}

fn strip_secrets(auth: &mut Auth) {
    match auth {
        Auth::Basic(b) => b.password.clear(),
        Auth::Bearer(t) => t.clear(),
        Auth::OAuth2(o) => {
            o.client_secret.clear();
            o.password.clear();
        }
        Auth::Inherit | Auth::None => {}
    }
}

/// Blanks the value of headers that usually carry credentials (`Authorization`, `X-API-Key`, cookies...).
fn strip_header_secrets(headers: &mut [KeyValue]) {
    // Broad on purpose ("auth", "key"): blanking a harmless `Idempotency-Key` is cheaper than leaking `X-Auth`.
    const HINTS: [&str; 6] = ["auth", "cookie", "token", "secret", "password", "key"];
    for header in headers {
        let name = header.key.to_ascii_lowercase();
        if HINTS.iter().any(|hint| name.contains(hint)) {
            header.value.clear();
        }
    }
}

/// Clears environment values, Auth secrets and the values of credential-like headers. Other headers, URLs, queries and
/// bodies still need review.
fn without_secrets(project: &Project) -> Project {
    let mut project = project.clone();
    for env in &mut project.environments {
        env.values = blank(&env.values);
    }
    strip_secrets(&mut project.auth);
    strip_header_secrets(&mut project.headers);
    for folder in &mut project.folders {
        strip_secrets(&mut folder.auth);
        strip_header_secrets(&mut folder.headers);
    }
    let in_folders = project.folders.iter_mut().flat_map(|f| f.requests.iter_mut());
    for request in project.requests.iter_mut().chain(in_folders) {
        strip_secrets(&mut request.auth);
        strip_header_secrets(&mut request.headers);
    }
    project
}

fn write(envelope: &Envelope) -> String {
    serde_json::to_string_pretty(envelope).expect("a project always serialises")
}

/// With `include_values` false, removes environment values and Auth secrets only.
pub fn export(project: &Project, include_values: bool) -> String {
    write(&Envelope {
        format: FORMAT.into(),
        version: SINGLE,
        project: Some(if include_values {
            project.clone()
        } else {
            without_secrets(project)
        }),
        projects: vec![],
    })
}

/// One project from a single-project file, all of them from an "export all" file. Each gets a fresh id, so importing
/// never overwrites a project that is already open.
pub fn import(text: &str) -> Result<Vec<Project>, ImportError> {
    let envelope: Envelope = serde_json::from_str(text).map_err(|e| {
        // A JSON file of another shape (no `format`) is "not ours", not a syntax error.
        match serde_json::from_str::<Value>(text) {
            Ok(_) => ImportError::NotAProject,
            Err(_) => ImportError::Invalid(e.to_string()),
        }
    })?;
    if envelope.format != FORMAT || envelope.version < SINGLE {
        return Err(ImportError::NotAProject);
    }
    if envelope.version > VERSION {
        return Err(ImportError::TooNew(envelope.version));
    }
    let projects = if envelope.version > SINGLE {
        envelope.projects
    } else {
        vec![envelope.project.ok_or(ImportError::NotAProject)?]
    };
    if projects.is_empty() {
        return Err(ImportError::NotAProject);
    }
    Ok(projects
        .into_iter()
        .map(|p| {
            let mut p = without_file_bodies(Project { id: new_id(), ..p });
            p.normalize_environments();
            p
        })
        .collect())
}

/// A file body is a path on the exporter's machine. It is dropped on import so a shared file cannot make Send upload a
/// local file of the importer; the user picks the file again in the request.
fn without_file_bodies(mut project: Project) -> Project {
    let folder_requests = project.folders.iter_mut().flat_map(|f| f.requests.iter_mut());
    for r in project.requests.iter_mut().chain(folder_requests) {
        if matches!(r.body, Body::File(_)) {
            r.body = Body::None;
        }
    }
    project
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{Basic, Environment, Folder, KeyValue, OAuth2, Request};

    #[test]
    fn native_envelopes_require_a_supported_version_and_project_payload() {
        for file in [
            r#"{"format":"fastapiclient-project","version":1}"#,
            r#"{"format":"fastapiclient-project","version":1,"project":null}"#,
            r#"{"format":"fastapiclient-project","version":0,"project":{}}"#,
        ] {
            assert_eq!(import(file), Err(ImportError::NotAProject));
        }
    }

    #[test]
    fn imported_environments_remain_independently_selectable() {
        let file = r#"{"format":"fastapiclient-project","version":1,"project":{"active_env":"Shared","environments":[{"name":"Shared","values":{"host":"dev"}},{"name":"Shared","values":{"host":"prod"}},{"name":"","values":{"host":"blank"}}]}}"#;
        let mut ws = crate::workspace::Workspace::new(import(file).unwrap());
        assert_eq!(ws.projects[0].active_values()["host"], "dev");
        ws.rename_env(1, "Production");
        assert_eq!(ws.active_env_index(), Some(0));
        assert_eq!(ws.projects[0].active_values()["host"], "dev");
        ws.set_active_env(Some(1));
        assert_eq!(ws.active_env_index(), Some(1));
        assert_eq!(ws.projects[0].active_values()["host"], "prod");
        ws.set_active_env(Some(2));
        assert_eq!(ws.projects[0].active_values()["host"], "blank");
        ws.set_active_env(None);
        assert!(ws.projects[0].active_values().is_null());
    }

    fn sample() -> Project {
        let mut p = Project::new("Shop");
        p.auth = Auth::Bearer("project-token".into());
        p.environments = vec![Environment {
            name: "Prod".into(),
            values: json!({"host": "api.shop.com", "db": {"port": 5432, "tags": ["a"]}}),
        }];
        p.active_env = "Prod".into();
        p.folders.push(Folder {
            name: "F".into(),
            auth: Auth::OAuth2(OAuth2 {
                client_id: "id".into(),
                client_secret: "shh".into(),
                password: "pw".into(),
                ..OAuth2::default()
            }),
            requests: vec![Request {
                name: "in-folder".into(),
                headers: vec![
                    KeyValue::new("X-A", "1"),
                    KeyValue::new("Authorization", "Bearer hdr-secret"),
                    KeyValue::new("X-Auth", "xauth-secret"),
                    KeyValue::new("Api_Key", "underscore-secret"),
                ],
                auth: Auth::Basic(Basic {
                    username: "ana".into(),
                    password: "hunter2".into(),
                }),
                ..Request::default()
            }],
            ..Folder::default()
        });
        p.requests.push(Request {
            name: "root".into(),
            auth: Auth::Bearer("request-token".into()),
            ..Request::default()
        });
        p
    }

    #[test]
    fn full_export_roundtrips_with_a_new_id() {
        let p = sample();
        let back = import(&export(&p, true)).unwrap().remove(0);
        assert_ne!(back.id, p.id);
        assert_eq!(
            Project {
                id: p.id.clone(),
                ..back
            },
            p
        );
    }

    #[test]
    fn export_without_values_keeps_keys_and_drops_secrets() {
        let text = export(&sample(), false);
        for secret in [
            "api.shop.com",
            "5432",
            "project-token",
            "request-token",
            "hunter2",
            "hdr-secret",
            "xauth-secret",
            "underscore-secret",
            "shh",
            "\"pw\"",
        ] {
            assert!(!text.contains(secret), "{secret} leaked:\n{text}");
        }
        let back = import(&text).unwrap().remove(0);
        assert_eq!(
            back.environments[0].values,
            json!({"host": "", "db": {"port": "", "tags": []}})
        );
        // Everything that is not a secret survives: names, usernames, ids, headers.
        assert_eq!(back.active_env, "Prod");
        assert_eq!(back.folders[0].requests[0].headers[0].value, "1");
        assert!(
            matches!(&back.folders[0].requests[0].auth, Auth::Basic(b) if b.username == "ana" && b.password.is_empty())
        );
        assert!(matches!(&back.folders[0].auth, Auth::OAuth2(o) if o.client_id == "id" && o.client_secret.is_empty()));
    }

    #[test]
    fn imported_api_key_headers_are_blanked_in_a_values_free_export() {
        let source = json!({"type": "collection.insomnia.rest/5.0", "collection": [
            {"name": "API", "method": "GET", "url": "http://unused", "authentication": {
                "type": "apikey", "key": "X-API-Key", "value": "literal-secret", "addTo": "header"}}
        ]});
        let project = crate::insomnia::import(&source.to_string()).unwrap();
        let text = export(&project, false);
        assert!(!text.contains("literal-secret"));
        let back = &import(&text).unwrap()[0].requests[0].headers[0];
        assert_eq!((back.key.as_str(), back.value.as_str()), ("X-API-Key", ""));
        assert!(export(&project, true).contains("literal-secret"));
    }

    #[test]
    fn imported_projects_lose_their_file_bodies_but_keep_the_rest() {
        let file = Request {
            name: "upload".into(),
            body: Body::File("/home/u/.ssh/id_rsa".into()),
            ..Request::default()
        };
        let mut p = sample();
        p.requests.push(file.clone());
        p.folders[0].requests.push(file);
        p.requests.push(Request {
            body: Body::Json("{}".into()),
            ..Request::default()
        });
        let got = &import(&export(&p, true)).unwrap()[0];
        let bodies: Vec<_> = got
            .requests
            .iter()
            .chain(got.folders.iter().flat_map(|f| &f.requests))
            .map(|r| r.body.clone())
            .collect();
        assert!(bodies.iter().all(|b| !matches!(b, Body::File(_))), "{bodies:?}");
        assert!(bodies.contains(&Body::Json("{}".into())));
        assert_eq!(got.requests.len(), p.requests.len(), "the requests themselves stay");
    }

    #[test]
    fn foreign_and_future_files_are_refused() {
        assert_eq!(import(r#"{"name": "x"}"#), Err(ImportError::NotAProject));
        assert_eq!(import("[1, 2]"), Err(ImportError::NotAProject));
        assert_eq!(
            import(r#"{"format": "other", "version": 1}"#),
            Err(ImportError::NotAProject)
        );
        assert!(matches!(import("{not json"), Err(ImportError::Invalid(_))));
        let future = format!(r#"{{"format": "{FORMAT}", "version": 99}}"#);
        assert_eq!(import(&future), Err(ImportError::TooNew(99)));
    }

    #[test]
    fn an_explicit_empty_project_imports_with_defaults() {
        let p = import(&format!(r#"{{"format": "{FORMAT}", "version": 1, "project": {{}}}}"#))
            .unwrap()
            .remove(0);
        assert!(p.requests.is_empty() && !p.id.is_empty());
    }

    #[test]
    fn legacy_multi_project_files_import_with_fresh_ids() {
        let (a, b) = (sample(), Project::new("Other"));
        let text = serde_json::json!({"format": FORMAT, "version": 2, "projects": [a, b]}).to_string();
        let back = import(&text).unwrap();
        assert_eq!(back.len(), 2);
        assert!(back[0].id != a.id && back[1].id != b.id && back[0].id != back[1].id);
        assert_eq!(back[0].name, "Shop");
        assert_eq!(back[0].folders, a.folders);
        assert_eq!(back[1].name, "Other");
    }

    #[test]
    fn single_exports_stay_version_1_and_an_empty_all_file_is_refused() {
        assert!(export(&sample(), true).contains("\"version\": 1"));
        assert!(!export(&sample(), true).contains("\"projects\""));
        let empty = serde_json::json!({"format": FORMAT, "version": 2, "projects": []}).to_string();
        assert_eq!(import(&empty), Err(ImportError::NotAProject));
    }
}
