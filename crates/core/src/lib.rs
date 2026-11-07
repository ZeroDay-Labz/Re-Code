//! Domain types shared by every re-code subsystem.
//!
//! This crate performs no I/O. Clients such as the TUI depend only on it, so the compiler
//! guarantees that an interface cannot run a tool or call a provider directly.

pub mod base64;
pub mod capability;
pub mod context;
pub mod extensions;
pub mod ids;
pub mod message;
pub mod model_facts;
pub mod protocol;
pub mod providers;
pub mod usage;
