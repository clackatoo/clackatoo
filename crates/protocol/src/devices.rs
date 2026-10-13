//! Devices, such as phones, that reach the daemon through its web app:
//! those paired with it, and those asking to be.
//!
//! A device pairs once, with the daemon's password, or by being allowed
//! from a window: it asks, the daemon shows the question in every window
//! as a [`PairingAsk`], and the device shows a short code, which the user
//! types into a window to allow it
//! ([`Request::AnswerPairing`](crate::Request::AnswerPairing)). A paired
//! device is then a client like any other, until it's revoked
//! ([`Request::RevokeDevice`](crate::Request::RevokeDevice)) or goes
//! unused for long enough to expire.

use serde::{Deserialize, Serialize};

/// A device paired with the daemon, as the list of them shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The daemon's number for it, which revoking it names. Kept on disk
    /// with the device, so it's the same from one run of the daemon to
    /// the next.
    pub id: u64,
    /// What it called itself when it paired: "iPhone".
    pub name: String,
    /// When it paired, in milliseconds since the Unix epoch.
    pub paired: u64,
    /// When it last connected, in milliseconds since the Unix epoch.
    pub used: u64,
    /// Who it was, as Tailscale said, when it paired through Tailscale.
    pub login: Option<String>,
    /// Whether it has asked to be sent notifications.
    pub notifications: bool,
    /// Whether it's connected now.
    pub connected: bool,
}

/// A device asking to pair, waiting to be allowed from a window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingAsk {
    /// What it calls itself: "iPhone".
    pub name: String,
    /// Where it asks from: who Tailscale says it is, or its address.
    pub from: String,
    /// When the question goes, unanswered, in milliseconds since the Unix
    /// epoch.
    pub until: u64,
}
