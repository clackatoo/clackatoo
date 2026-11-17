//! The MCP servers clackatoo manages for agents: their configuration,
//! and the proxy each chat reaches each of them through.
//!
//! The daemon grants each proxy the definition its chat may use. The
//! proxy holds the connection to the server, in a process of its own, so
//! a daemon that upgrades leaves the server's session as it was; it
//! connects again when the server goes, and asks the daemon's leave for
//! each call (see `proxy`).
pub(crate) mod config;
pub(crate) mod credentials;
pub(crate) mod import;
pub(crate) mod oauth;
pub(crate) mod proxy;
pub(crate) mod transport;

pub use proxy::relay;

#[cfg(test)]
mod tests;
