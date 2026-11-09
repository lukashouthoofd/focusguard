//! Reusable policy coordination primitives for the platform service hosts.

#[path = "enforcement.rs"]
pub mod enforcement;

#[cfg(any(target_os = "macos", all(test, unix)))]
pub mod unix_socket;

#[cfg(windows)]
pub mod app_enforcement;
