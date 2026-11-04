//! Frozen federation handover records, introduced in handover 43.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Federation {
    pub window: u64,
    pub changes: u64,
    pub next_board: u32,
    pub active: Option<u32>,
    pub peers: Vec<Peer>,
    pub typed_parked: Vec<([u8; 16], u64)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Peer {
    pub host: String,
    pub command: String,
    pub asked: bool,
    pub unshared: bool,
    pub expected: Option<[u8; 16]>,
    pub wire: Option<u32>,
    /// A frozen v1 view frame, always encoded at protocol 125. See
    /// server/relay/upgrade.rs; its fixture must remain readable forever.
    pub view: Vec<u8>,
    pub buffers: Vec<Buffer>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Buffer {
    pub local: u32,
    pub applied: Vec<(u64, u64, Option<u64>)>,
    /// The relay sequence, original window and its sequence. Its window
    /// retains the change itself and resends only if it never landed.
    pub pending: Option<(u64, u64, u64)>,
}
