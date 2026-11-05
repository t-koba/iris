//! The Iris application: state, messages, update logic, views and the `AppRuntime` that ties
//! the other crates together.

pub mod app;
pub mod commands;
pub mod config;
pub mod format;
pub mod i18n;
pub mod input;
#[cfg(target_os = "macos")]
pub mod macos_menu;
pub mod messages;
pub mod observability;
pub mod remote_transport;
pub mod runtime;
pub mod state;
#[cfg(test)]
mod test_support;
pub mod undo;
pub mod update;
pub mod views;
pub mod widgets;
