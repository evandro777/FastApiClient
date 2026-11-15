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

    pub fn active_values(&self) -> &Value {
        self.environments
            .iter()
            .find(|e| e.name == self.active_env)
            .map_or(&Value::Null, |e| &e.values)
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
    fn ids_are_unique_and_file_safe() {
        let (a, b) = (new_id(), new_id());
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
