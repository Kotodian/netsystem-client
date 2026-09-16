//! Failures of the stats client, its mapping, and its family projections.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use hammer_stats_protocol::protocol::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("connect to stats segment socket `{path}`: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stats segment socket path `{path}` exceeds {max} bytes")]
    SocketPathTooLong { path: PathBuf, max: usize },
    #[error("stats segment handoff received {received_fds} descriptors")]
    AncillaryData {
        received_fds: usize,
        truncated: bool,
    },
    #[error("receive the stats segment descriptor: {source}")]
    Receive {
        #[source]
        source: io::Error,
    },
    #[error("the stats listener did not hand over a segment descriptor within {waited:?}")]
    HandoffTimeout { waited: Duration },
    #[error("read the stats segment size: {source}")]
    Fstat {
        #[source]
        source: io::Error,
    },
    #[error("stats segment publishes size {size}, which is not a positive length")]
    InvalidSegmentSize { size: i64 },
    #[error("map the stats segment: {source}")]
    Mapping {
        #[source]
        source: io::Error,
    },
    #[error("stats segment layout: {source}")]
    Protocol {
        #[from]
        source: ProtocolError,
    },
    #[error("stats segment changed during {operation} {attempts} times")]
    RetryExhausted {
        operation: &'static str,
        attempts: usize,
    },
    #[error("`{name}` is not in the stats segment directory")]
    MetricNotFound { name: String },
    #[error("`{name}` is a {directory_type} entry the reader does not decode")]
    UnsupportedDirectoryType {
        name: String,
        directory_type: &'static str,
    },
    #[error("`{name}` is a {actual} entry, expected {expected}")]
    UnexpectedMetricType {
        name: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("`{name}` has {actual} rows, expected {expected}")]
    UnexpectedRowCount {
        name: String,
        expected: usize,
        actual: usize,
    },
    #[error("`{name}` has {actual} columns, expected {expected}")]
    UnexpectedColumnCount {
        name: String,
        expected: usize,
        actual: usize,
    },
    #[error(
        "`{name}` has {columns} columns but `/sys/num_worker_threads` is {worker_thread_count}"
    )]
    WorkerCountMismatch {
        name: String,
        worker_thread_count: u64,
        columns: usize,
    },
    #[error("symlink `{name}` did not resolve to a value within {depth} hops")]
    SymlinkCycle { name: String, depth: usize },
}
