//! OS-specific services: mount tables, trash, credential store, file watching, file times and
//! default-application registration.

pub mod copy;
pub mod credential_store;
pub mod default_app;
pub mod error;
pub mod file_times;
pub mod file_watch;
pub mod mount;
pub mod path;
pub mod trash;
