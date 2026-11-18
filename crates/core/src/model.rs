//! Data model: Project > Folder (optional) > Request. Every struct is `#[serde(default)]` so files written by an
//! older version keep loading when a field is added.
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
    pub description: String,
    pub enabled: bool,
}

impl Default for KeyValue {
    fn default() -> Self {
        Self {
            key: String::new(),
            value: String::new(),
            description: String::new(),
            enabled: true,
        }
    }
}

impl KeyValue {
    pub fn new(key: &str, value: &str) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            ..Self::default()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(tag = "type", content = "value")]
pub enum Body {
    #[default]
    None,
    FormData(Vec<KeyValue>),
    FormUrl(Vec<KeyValue>),
    /// JSONC is accepted; comments are stripped when the request is sent.
    Json(String),
    Xml(String),
    /// Path of the file sent as the raw body.
    File(String),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub enum Grant {
    #[default]
    ClientCredentials,
    Password,
    AuthorizationCode,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct OAuth2 {
    pub grant: Grant,
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: String,
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Basic {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(tag = "type", content = "value")]
pub enum Auth {
    /// Use the parent's auth (Folder, then Project).
    #[default]
    Inherit,
    None,
    Basic(Basic),
    Bearer(String),
    OAuth2(OAuth2),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Request {
    pub name: String,
    /// Any HTTP method; kept as text so custom methods work.
    pub method: String,
    pub url: String,
    pub query: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub auth: Auth,
    pub body: Body,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            name: String::new(),
            method: "GET".into(),
            url: String::new(),
            query: vec![],
            headers: vec![],
            auth: Auth::Inherit,
            body: Body::None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Folder {
    pub name: String,
    /// Index into `Project::folders` of the enclosing folder; `None` for a top-level one. A flat list keeps the
    /// selection (`Sel`) and the saved settings unchanged.
    pub parent: Option<usize>,
    pub headers: Vec<KeyValue>,
    pub auth: Auth,
    pub requests: Vec<Request>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Environment {
    pub name: String,
    /// A JSON object; nested values are reached with `{{a.b}}`.
    pub values: Value,
}

#[derive(Debug, PartialEq)]
pub enum EnvError {
    /// The JSON parser's message (English, with line and column).
    Invalid(String),
    /// Valid JSON, but not `{ ... }`.
    NotObject,
}

impl Environment {
    pub fn parse(text: &str) -> Result<Value, EnvError> {
        match serde_json::from_str::<Value>(text) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(_) => Err(EnvError::NotObject),
            Err(e) => Err(EnvError::Invalid(e.to_string())),
        }
    }

    /// The text shown in the editor, indented with 4 spaces like the rest of the app; a file without `values` reads
    /// as an empty object.
    pub fn pretty(&self) -> String {
        let mut out = vec![];
        let format = serde_json::ser::PrettyFormatter::with_indent(b"    ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut out, format);
        match &self.values {
            Value::Null => return "{}".into(),
            v => v.serialize(&mut serializer).ok(),
        };
        String::from_utf8(out).unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Project {
    /// Also the file name in the store, so it only ever holds `[A-Za-z0-9_-]`.
    pub id: String,
    pub name: String,
    pub headers: Vec<KeyValue>,
    pub auth: Auth,
    pub folders: Vec<Folder>,
    /// Requests directly under the project (the folder level is optional).
    pub requests: Vec<Request>,
    pub environments: Vec<Environment>,
    /// Name of the active environment; "" means none.
    pub active_env: String,
}

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            ..Self::default()
        }
    }

    /// Folder indexes from the outermost ancestor down to `folder` itself. Stops on a cycle or a dangling parent,
    /// because the file can be edited by hand.
    pub fn folder_chain(&self, folder: usize) -> Vec<usize> {
        let mut chain = vec![];
        let mut at = Some(folder);
        while let Some(i) = at.filter(|i| *i < self.folders.len() && !chain.contains(i)) {
            chain.push(i);
            at = self.folders[i].parent;
        }
        chain.reverse();
        chain
    }

    /// The first request called `name` with its parent folders (outermost first): those directly under the project,
    /// then folder by folder. Shortcut: names are not unique, so a duplicate is never reachable; rename it.
    pub fn find_request(&self, name: &str) -> Option<(Vec<&Folder>, &Request)> {
        if let Some(r) = self.requests.iter().find(|r| r.name == name) {
            return Some((vec![], r));
        }
        let i = self
            .folders
            .iter()
            .position(|f| f.requests.iter().any(|r| r.name == name))?;
        let r = self.folders[i].requests.iter().find(|r| r.name == name)?;
        Some((self.folder_chain(i).into_iter().map(|j| &self.folders[j]).collect(), r))
    }

    /// Whether `request` is the one `find_request` reaches by its name. Names can repeat, and only that one may own the
    /// cached `$response` value, or another request's token would be used.
    pub fn owns_response_name(&self, request: &Request) -> bool {
        self.find_request(&request.name)
            .is_some_and(|(_, found)| found == request)
    }

    pub fn active_values(&self) -> &Value {
        if self.active_env.is_empty() {
            return &Value::Null;
        }
        self.environments
            .iter()
            .find(|e| e.name == self.active_env)
            .map_or(&Value::Null, |e| &e.values)
    }

    pub(crate) fn unique_env_name(&self, wanted: &str, skip: Option<usize>) -> String {
        let taken = |name: &str| {
            self.environments
                .iter()
                .enumerate()
                .any(|(i, env)| Some(i) != skip && env.name.eq_ignore_ascii_case(name))
        };
        if !taken(wanted) {
            return wanted.into();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{wanted} {n}");
            if !taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Names identify environments. Preserve the active entry while repairing external names.
    pub(crate) fn normalize_environments(&mut self) {
        let active = self
            .environments
            .iter()
            .position(|env| !self.active_env.is_empty() && env.name == self.active_env);
        for env in &mut self.environments {
            env.name = env.name.trim().to_owned();
        }
        // Walk backwards so the first duplicate keeps its name; existing suffixes remain reserved.
        for i in (0..self.environments.len()).rev() {
            let wanted = if self.environments[i].name.is_empty() {
                (i + 1).to_string()
            } else {
                self.environments[i].name.clone()
            };
            self.environments[i].name = self.unique_env_name(&wanted, Some(i));
        }
        self.active_env = active.map(|i| self.environments[i].name.clone()).unwrap_or_default();
    }
}

/// Unique enough for local files: clock nanoseconds plus a counter, no uuid dependency.
pub fn new_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_request_of_a_repeated_name_owns_it() {
        let named = |url: &str| Request {
            name: "Login".into(),
            url: url.into(),
            ..Request::default()
        };
        let mut p = Project::new("p");
        p.folders.push(Folder::default());
        p.folders.push(Folder::default());
        p.folders[0].requests.push(named("http://first"));
        p.folders[1].requests.push(named("http://second"));
        assert!(p.owns_response_name(&p.folders[0].requests[0]));
        assert!(!p.owns_response_name(&p.folders[1].requests[0]));
    }

    #[test]
    fn old_files_still_load() {
        let p: Project = serde_json::from_str(r#"{"name":"x","requests":[{"url":"http://a"}]}"#).unwrap();
        assert_eq!(p.name, "x");
        assert_eq!(p.requests[0].method, "GET");
        assert_eq!(p.requests[0].auth, Auth::Inherit);
        let p: Project = serde_json::from_str(r#"{"folders":[{"name":"f"}]}"#).unwrap();
        assert_eq!(p.folders[0].parent, None, "a folder without `parent` stays top level");
        let kv: KeyValue = serde_json::from_str(r#"{"key":"k"}"#).unwrap();
        assert!(kv.enabled, "a key/value without `enabled` must stay active");
    }

    #[test]
    fn body_and_auth_roundtrip() {
        let r = Request {
            body: Body::FormData(vec![KeyValue::new("a", "1")]),
            auth: Auth::OAuth2(OAuth2 {
                grant: Grant::AuthorizationCode,
                ..OAuth2::default()
            }),
            ..Request::default()
        };
        let back: Request = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn environment_text_must_be_a_json_object() {
        assert!(Environment::parse(r#"{"host": "x", "db": {"port": 1}}"#).is_ok());
        assert_eq!(Environment::parse("[1]"), Err(EnvError::NotObject));
        assert!(matches!(Environment::parse(r#"{"host": "#), Err(EnvError::Invalid(_))));
        assert_eq!(Environment::default().pretty(), "{}");
        let env = Environment {
            name: "x".into(),
            values: serde_json::json!({"db": {"port": 1}}),
        };
        assert_eq!(env.pretty(), "{\n    \"db\": {\n        \"port\": 1\n    }\n}");
    }

    #[test]
    fn environment_numbers_survive_editing_and_project_roundtrips() {
        for number in [
            "12345678901234567890123",
            "-12345678901234567890123",
            "0.12345678901234567890123",
        ] {
            let env = Environment {
                name: "Exact".into(),
                values: Environment::parse(&format!("{{\"id\":{number}}}")).unwrap(),
            };
            let edited = Environment::parse(&env.pretty()).unwrap();
            assert_eq!(crate::resolve::substitute("{{id}}", &edited).0, number);
            let mut project = Project::new("Exact numbers");
            project.active_env = env.name.clone();
            project.environments.push(env);
            let saved = serde_json::to_string(&project).unwrap();
            let restored: Project = serde_json::from_str(&saved).unwrap();
            assert_eq!(crate::resolve::substitute("{{id}}", restored.active_values()).0, number);
            let exported = crate::exchange::export(&restored, true);
            let imported = crate::exchange::import(&exported).unwrap();
            assert_eq!(
                crate::resolve::substitute("{{id}}", imported[0].active_values()).0,
                number
            );
        }
    }

    #[test]
    fn external_environment_names_are_unique_and_keep_the_active_entry() {
        for active in ["Shared", " Shared 2 ", "", "missing", "   "] {
            let mut p = Project::new("Names");
            p.environments = ["Shared", "shared", " Shared 2 ", "", "   ", "6", "SHARED"]
                .iter()
                .enumerate()
                .map(|(i, name)| Environment {
                    name: (*name).into(),
                    values: serde_json::json!({"id": i}),
                })
                .collect();
            p.active_env = active.into();
            let expected = p.active_values().clone();
            p.normalize_environments();
            let mut seen = std::collections::HashSet::new();
            for env in &p.environments {
                assert!(!env.name.is_empty());
                assert_eq!(env.name, env.name.trim());
                assert!(seen.insert(env.name.to_ascii_lowercase()));
            }
            assert_eq!(p.environments[0].name, "Shared");
            assert_eq!(p.active_values(), &expected);
            let normalized = p.clone();
            p.normalize_environments();
            assert_eq!(p, normalized);
        }
    }

    #[test]
    fn no_environment_never_matches_an_unnamed_entry() {
        let mut p = Project::new("No environment");
        p.environments.push(Environment {
            name: String::new(),
            values: serde_json::json!({"host": "production"}),
        });
        assert!(p.active_values().is_null());
        let mut ws = crate::workspace::Workspace::new(vec![p]);
        ws.set_active_env(None);
        assert_eq!(ws.active_env_index(), None);
        assert!(ws.projects[0].active_values().is_null());
    }

    #[test]
    fn ids_are_unique_and_file_safe() {
        let (a, b) = (new_id(), new_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
