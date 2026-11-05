//! Persists transfer jobs in SQLite so they resume after a restart.

pub mod repository;
pub mod schema;
pub mod writer_task;
