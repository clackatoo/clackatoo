//! Managed MCP configuration and isolated session proxies.
//!
//! The daemon grants each proxy its chat's effective configuration. The
//! proxy owns transport state, so live daemon upgrades leave upstream MCP
//! sessions intact. It rechecks grants before requests and while idle.
pub(crate) mod config;
pub(crate) mod credentials;
pub(crate) mod import;
pub(crate) mod oauth;
pub(crate) mod proxy;
pub(crate) mod transport;

pub use proxy::relay;

#[cfg(test)]
mod tests;
