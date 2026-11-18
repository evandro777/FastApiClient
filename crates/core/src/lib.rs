//! UI-independent logic: settings, the project model, inheritance and storage. Keep Slint out of this crate so a CLI
//! can reuse it.
pub mod chain;
pub mod exchange;
pub mod files;
pub mod find;
pub mod format;
pub mod http;
pub mod insomnia;
pub mod model;
pub mod resolve;
pub mod settings;
pub mod store;
pub mod workspace;
