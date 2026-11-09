//! The re-code terminal interface.
//!
//! The interface is a pure function of events: the same [`EventKind`] values drive it whether they
//! come from the engine or from the recorded demo.
//!
//! [`EventKind`]: recode_core::protocol::EventKind

pub mod app;
pub mod background;
pub mod complete;
pub mod demo;
pub mod export;
pub mod extensions_view;
pub mod form;
pub mod home;
pub mod input;
pub mod install_view;
pub mod link;
pub mod markdown;
pub mod onboarding;
mod run;
pub mod runtimes_view;
pub mod sanitize;
pub mod status;
pub mod terminal_hint;
pub mod theme;
pub mod theme_file;
pub mod transcript;
pub mod tree;
pub mod view;
pub mod welcome;
pub mod widgets;
pub mod wrap;

pub use run::{run_demo, run_interactive};
