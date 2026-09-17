//! `/err/<node>/<error>` projection: one alias per registered node error,
//! pointing at that error's column of the server's `/node/errors` counter
//! vector.
//!
//! The server publishes the vector and its aliases under the names VPP's
//! `vlib_register_errors` uses (`third_party/vpp/src/vlib/error.c:182-186`).
//! VPP's own client reads the aliases and nothing else: `set_errors` filters
//! the directory by the `/err/` prefix (`vpp_stats.py:236-248`), so this
//! projection does the same — list the names once, read every alias, and treat
//! an alias that vanishes between the listing and the read as an ordinary
//! absence. Names the server never publishes (`/err/<node>` without an error
//! segment) are skipped, not reported as a failure.
//!
//! Unlike the `/sys/*` families, records are visible immediately: the server's
//! record point writes the cell, so the next read sees it without waiting for a
//! collector round.
//!
//! An absent family is `None`: no server node declared errors, which is the
//! state VPP expresses by never creating the entry (`error.c:135,158-159`).

use std::time::Instant;

use hammer_stats_protocol::protocol::MetricValue;

use crate::error::Error;
use crate::provider::StatsProvider;
use crate::reader::StatsReader;

/// One node error's counters, one value per runtime thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeErrorCounters {
    /// The node the error belongs to, from `/err/<node>/<error>`.
    pub node: String,
    /// The error's name inside that node.
    pub error: String,
    /// Counts in thread order, thread zero first; the length is the thread
    /// count the server published.
    pub counts: Vec<u64>,
}

impl NodeErrorCounters {
    /// The error's total across every thread.
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }
}

/// Every node error the segment published, ordered by `(node, error)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeErrorStats {
    pub errors: Vec<NodeErrorCounters>,
    /// When this report was produced, from the client's monotonic clock. The
    /// entries are independent reads, not one instant.
    pub sampled_at: Instant,
}

impl NodeErrorStats {
    /// The counters of one `(node, error)` pair.
    pub fn error(&self, node: &str, error: &str) -> Option<&NodeErrorCounters> {
        self.errors
            .iter()
            .find(|counters| counters.node == node && counters.error == error)
    }

    /// Every error one node published, in the report's order.
    pub fn node<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a NodeErrorCounters> + 'a {
        self.errors
            .iter()
            .filter(move |counters| counters.node == name)
    }

    /// The total across every published error and thread.
    pub fn total(&self) -> u64 {
        self.errors.iter().map(NodeErrorCounters::total).sum()
    }
}

/// Projects `/err` into `Option<NodeErrorStats>`.
pub struct NodeErrorStatsProvider;

impl StatsProvider for NodeErrorStatsProvider {
    const PREFIX: &'static str = "/err";
    type Report = Option<NodeErrorStats>;

    fn report<R: StatsReader>(reader: &R) -> Result<Option<NodeErrorStats>, Error> {
        let names = reader.names()?;
        let mut aliases: Vec<&String> = names
            .iter()
            .filter(|name| {
                name.strip_prefix(Self::PREFIX)
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .collect();
        if aliases.is_empty() {
            return Ok(None);
        }
        aliases.sort();
        let sampled_at = Instant::now();

        let mut errors = Vec::with_capacity(aliases.len());
        for name in aliases {
            let Some((node, error)) = alias_parts(name) else {
                continue;
            };
            let counts = match read_counts(reader, name) {
                Ok(counts) => counts,
                // The alias disappeared between the listing and the read; VPP's
                // client swallows the same `KeyError`.
                Err(Error::MetricNotFound { .. }) => continue,
                Err(error) => return Err(error),
            };
            errors.push(NodeErrorCounters {
                node: node.to_owned(),
                error: error.to_owned(),
                counts,
            });
        }
        errors.sort_by(|left, right| (&left.node, &left.error).cmp(&(&right.node, &right.error)));
        Ok(Some(NodeErrorStats { errors, sampled_at }))
    }
}

/// Splits `/err/<node>/<error>` into its two names; a name without both
/// segments is not a published alias and is skipped.
fn alias_parts(name: &str) -> Option<(&str, &str)> {
    let rest = name
        .strip_prefix(NodeErrorStatsProvider::PREFIX)?
        .strip_prefix('/')?;
    let (node, error) = rest.split_once('/')?;
    (!node.is_empty() && !error.is_empty()).then_some((node, error))
}

/// One alias's value: one cell per runtime thread, in thread order.
fn read_counts<R: StatsReader>(reader: &R, name: &str) -> Result<Vec<u64>, Error> {
    let rows = match reader.read(name)? {
        MetricValue::Simple(rows) => rows,
        value => {
            return Err(Error::UnexpectedMetricType {
                name: name.to_owned(),
                expected: "counter_vector_simple",
                actual: metric_type_name(&value),
            });
        }
    };
    let mut counts = Vec::with_capacity(rows.len());
    for (row, values) in rows.iter().enumerate() {
        let [value] = values.as_slice() else {
            return Err(Error::UnexpectedColumnCount {
                name: format!("{name} row {row}"),
                expected: 1,
                actual: values.len(),
            });
        };
        counts.push(*value);
    }
    Ok(counts)
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
