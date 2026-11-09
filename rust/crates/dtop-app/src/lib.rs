//! dtop's application layer: pure state and reducer, the theme editor, views as pure functions
//! of state, and the runtime (refresher, controller, entry point). Side effects run only in the
//! runtime, through `AppCommand`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod controller;
pub mod entry;
pub mod list;
pub mod log_policy;
pub mod once;
pub mod operation;
pub mod reducer;
pub mod refresher;
pub mod state;
pub mod theme_editor;
pub mod theme_files;
pub mod views;

pub use entry::run;
