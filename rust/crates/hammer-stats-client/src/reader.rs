//! The read capability every family projection depends on.

use hammer_stats_protocol::protocol::MetricValue;

use crate::error::Error;

/// The whole capability a provider may use: list names, read one value.
///
/// [`StatsClient`](crate::StatsClient) is the production implementation; tests
/// implement it over an in-memory fixture, so a projection never needs a socket
/// or a mapping.
pub trait StatsReader {
    fn names(&self) -> Result<Vec<String>, Error>;
    fn read(&self, name: &str) -> Result<MetricValue, Error>;
}
