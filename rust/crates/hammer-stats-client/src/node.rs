//! `/sys/node` projection: the node-name vector plus the four per-thread
//! counter vectors, one column per graph node.
//!
//! This family exists only when the server's `per_node_counters` switch is on,
//! so the report is an `Option`: an absent family is an ordinary result, a
//! malformed one is an error. The counters are the server's per-thread cells
//! (`calls`, `vectors`, `clocks` in raw CPU counter ticks) plus the suspend
//! count of process nodes; only thread zero's row has `suspends`, and a process
//! node's dispatch counters stay zero.

use std::time::Instant;

use hammer_stats_protocol::protocol::MetricValue;

use crate::error::Error;
use crate::provider::StatsProvider;
use crate::reader::StatsReader;

const NAMES: &str = "/sys/node/names";
const CLOCKS: &str = "/sys/node/clocks";
const VECTORS: &str = "/sys/node/vectors";
const CALLS: &str = "/sys/node/calls";
const SUSPENDS: &str = "/sys/node/suspends";
const WORKER_THREAD_COUNT: &str = "/sys/num_worker_threads";

/// One of the four counters the family publishes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeCounter {
    Clocks,
    Vectors,
    Calls,
    Suspends,
}

impl NodeCounter {
    /// The counter's name in the directory and in its `/nodes/<node>/<name>`
    /// alias.
    pub fn name(self) -> &'static str {
        match self {
            Self::Clocks => "clocks",
            Self::Vectors => "vectors",
            Self::Calls => "calls",
            Self::Suspends => "suspends",
        }
    }
}

/// One node's four counters, one value per thread in thread order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeUsage {
    pub name: String,
    /// Raw CPU counter ticks; not seconds, and only comparable on this host.
    pub clocks: Vec<u64>,
    pub vectors: Vec<u64>,
    pub calls: Vec<u64>,
    pub suspends: Vec<u64>,
}

impl NodeUsage {
    /// The values of one counter, in thread order.
    pub fn counter(&self, counter: NodeCounter) -> &[u64] {
        match counter {
            NodeCounter::Clocks => &self.clocks,
            NodeCounter::Vectors => &self.vectors,
            NodeCounter::Calls => &self.calls,
            NodeCounter::Suspends => &self.suspends,
        }
    }
}

/// One reading of the whole family: row = thread, column = node slot.
///
/// The five entries are five independent reads, not one instant: the server
/// deliberately does not stop its Data Workers to sample them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeStats {
    /// Node names by slot; the column order of every counter vector.
    pub names: Vec<String>,
    pub clocks: Vec<Vec<u64>>,
    pub vectors: Vec<Vec<u64>>,
    pub calls: Vec<Vec<u64>>,
    pub suspends: Vec<Vec<u64>>,
    /// When this report was produced, from the client's monotonic clock.
    pub sampled_at: Instant,
}

impl NodeStats {
    /// The number of rows: thread zero plus the Data Workers.
    pub fn thread_count(&self) -> usize {
        self.clocks.len()
    }

    /// The counter vector of one counter, `row → column → value`.
    pub fn counter(&self, counter: NodeCounter) -> &[Vec<u64>] {
        match counter {
            NodeCounter::Clocks => &self.clocks,
            NodeCounter::Vectors => &self.vectors,
            NodeCounter::Calls => &self.calls,
            NodeCounter::Suspends => &self.suspends,
        }
    }

    /// One node's four counters, taken from the column its name occupies.
    ///
    /// A name the vector does not carry, or one it carries twice, has no single
    /// column, so it yields `None` rather than an arbitrary pick.
    pub fn node(&self, name: &str) -> Option<NodeUsage> {
        let mut matching = self
            .names
            .iter()
            .enumerate()
            .filter(|(_, published)| published.as_str() == name);
        let (slot, _) = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        Some(NodeUsage {
            name: name.to_owned(),
            clocks: column(&self.clocks, slot),
            vectors: column(&self.vectors, slot),
            calls: column(&self.calls, slot),
            suspends: column(&self.suspends, slot),
        })
    }
}

/// Projects `/sys/node` into `Option<NodeStats>`.
pub struct NodeStatsProvider;

impl StatsProvider for NodeStatsProvider {
    const PREFIX: &'static str = "/sys/node";
    type Report = Option<NodeStats>;

    fn report<R: StatsReader>(reader: &R) -> Result<Option<NodeStats>, Error> {
        let names = reader.names()?;
        if !names.iter().any(|name| name == NAMES) {
            return Ok(None);
        }
        let sampled_at = Instant::now();
        let node_names = read_node_names(reader, NAMES)?;
        let clocks = read_counter(reader, CLOCKS)?;
        let vectors = read_counter(reader, VECTORS)?;
        let calls = read_counter(reader, CALLS)?;
        let suspends = read_counter(reader, SUSPENDS)?;

        let rows = clocks.len();
        for (name, counter) in [(VECTORS, &vectors), (CALLS, &calls), (SUSPENDS, &suspends)] {
            if counter.len() != rows {
                return Err(Error::UnexpectedRowCount {
                    name: name.to_owned(),
                    expected: rows,
                    actual: counter.len(),
                });
            }
        }
        for (name, counter) in [
            (CLOCKS, &clocks),
            (VECTORS, &vectors),
            (CALLS, &calls),
            (SUSPENDS, &suspends),
        ] {
            for (row, values) in counter.iter().enumerate() {
                if values.len() != node_names.len() {
                    return Err(Error::UnexpectedColumnCount {
                        name: format!("{name} row {row}"),
                        expected: node_names.len(),
                        actual: values.len(),
                    });
                }
            }
        }
        let worker_thread_count = read_u64(reader, WORKER_THREAD_COUNT)?;
        let expected_rows = usize::try_from(worker_thread_count)
            .expect("a worker thread count fits usize")
            .checked_add(1)
            .expect("thread count plus thread zero does not overflow");
        if rows != expected_rows {
            return Err(Error::WorkerCountMismatch {
                name: CLOCKS.to_owned(),
                worker_thread_count,
                columns: rows,
            });
        }

        Ok(Some(NodeStats {
            names: node_names,
            clocks,
            vectors,
            calls,
            suspends,
            sampled_at,
        }))
    }
}

/// One node's column across every thread.
fn column(counter: &[Vec<u64>], slot: usize) -> Vec<u64> {
    counter.iter().map(|row| row[slot]).collect()
}

/// The node-name vector, which publishes one name per node slot.
fn read_node_names<R: StatsReader>(reader: &R, name: &str) -> Result<Vec<String>, Error> {
    match reader.read(name)? {
        MetricValue::Names(names) => Ok(names),
        value => Err(Error::UnexpectedMetricType {
            name: name.to_owned(),
            expected: "name_vector",
            actual: metric_type_name(&value),
        }),
    }
}

/// One counter vector as `row → column → value`.
fn read_counter<R: StatsReader>(reader: &R, name: &str) -> Result<Vec<Vec<u64>>, Error> {
    match reader.read(name)? {
        MetricValue::Simple(rows) => Ok(rows),
        value => Err(Error::UnexpectedMetricType {
            name: name.to_owned(),
            expected: "counter_vector_simple",
            actual: metric_type_name(&value),
        }),
    }
}

fn read_u64<R: StatsReader>(reader: &R, name: &str) -> Result<u64, Error> {
    match reader.read(name)? {
        MetricValue::Scalar(value) | MetricValue::Gauge(value) => Ok(value),
        value => Err(Error::UnexpectedMetricType {
            name: name.to_owned(),
            expected: "scalar_index or gauge",
            actual: metric_type_name(&value),
        }),
    }
}

fn metric_type_name(value: &MetricValue) -> &'static str {
    match value {
        MetricValue::Scalar(_) => "scalar_index",
        MetricValue::Gauge(_) => "gauge",
        MetricValue::Simple(_) => "counter_vector_simple",
        MetricValue::Combined(_) => "counter_vector_combined",
        MetricValue::Names(_) => "name_vector",
        MetricValue::Histogram(_) => "histogram_log2",
        MetricValue::Ring(_) => "ring_buffer",
    }
}
