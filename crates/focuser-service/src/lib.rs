//! Reusable policy coordination primitives for the platform service hosts.

#[path = "enforcement.rs"]
pub mod enforcement;

#[cfg(unix)]
pub mod unix_socket;

#[cfg(windows)]
pub mod app_enforcement;
