//! Typed RPC services built on the transport-only Binary API client.

mod session;
mod vpe;

pub use session::{AppNamespaceAddDelError, NamespaceService};
pub use vpe::{VpeError, VpeService};
