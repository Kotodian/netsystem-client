//! Typed RPC services built on the transport-only Binary API client.

mod vpe;

pub use vpe::{VpeError, VpeService};
