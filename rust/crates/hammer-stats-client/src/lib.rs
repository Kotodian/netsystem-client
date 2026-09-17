//! External stats segment client: a read-only mechanism plus family
//! projections.
//!
//! The mechanism ([`StatsClient`]) connects to the stats segment socket,
//! receives the segment descriptor, maps the segment read-only, and serves
//! [`StatsReader`]: `names()` and `read(name)`. It knows no family semantics.
//!
//! A family projection is a [`StatsProvider`] with a typed report. The standard
//! families live one per module: [`memory`] for `/mem`, [`system`] for `/sys`,
//! [`buffer_pools`] for `/buffer-pools`, [`node`] for `/sys/node` and its
//! `/nodes/<node>/<counter>` aliases.
//! Adding a family adds a provider, not a `StatsClient` method.

pub mod buffer_pools;
mod client;
pub mod error;
pub mod memory;
pub mod node;
pub mod provider;
mod reader;
pub mod system;

pub use buffer_pools::{BufferPoolStats, BufferPoolStatsProvider, BufferPoolUsage};
pub use client::StatsClient;
pub use error::Error;
pub use hammer_stats_protocol::protocol::MetricValue;
pub use memory::{MemoryHeapUsage, MemoryStats, MemoryStatsProvider};
pub use node::{NodeCounter, NodeStats, NodeStatsProvider, NodeUsage};
pub use provider::StatsProvider;
pub use reader::StatsReader;
pub use system::{SystemStats, SystemStatsProvider};
