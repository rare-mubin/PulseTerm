//! SSH transport layer: connection setup, host-key pinning, and PTY channels.
//!
//! Nothing in here knows about gpui. It exposes plain async functions that are
//! safe to await from the UI executor (see `runtime`) plus a `flume` receiver
//! per pty for output, which the terminal views drive themselves.

pub mod client;
pub mod exec;
pub mod known_hosts;
pub mod pty;
pub mod sftp;

pub use client::{connect, AuthMethod, ConnectParams, Connection};

