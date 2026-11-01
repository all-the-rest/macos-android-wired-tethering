//! macOS-specific frame I/O and network integration.
//!
//! All privileged operations are exposed as narrow functions that are called
//! by the `android-tether-helper` binary.  The CLI itself only receives an
//! already-open BPF descriptor and interface names.

#![cfg(target_os = "macos")]

pub mod bpf;
pub mod command;
pub mod feth;
pub mod ipc;
pub mod service;
