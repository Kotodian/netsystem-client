//! `/mem` projection: one entry per memory heap, seven columns each.
//!
//! The column order is the server's `mem` family contract: `total`, `used`,
//! `free`, `used_mmap`, `max_allocated`, `free_chunk_count`, `releasable`.

use hammer_stats_protocol::protocol::MetricValue;

use crate::error::Error;
use crate::provider::StatsProvider;
use crate::reader::StatsReader;

/// One memory heap as the server last sampled it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryHeapUsage {
    pub name: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub used_mmap_bytes: u64,
    pub max_allocated_bytes: u64,
    pub free_chunk_count: u64,
    pub releasable_bytes: u64,
}

/// Every heap the segment publishes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryStats {
    pub heaps: Vec<MemoryHeapUsage>,
}

impl MemoryStats {
    /// Looks one heap up by its name without the `/mem/` prefix.
    pub fn heap(&self, name: &str) -> Option<&MemoryHeapUsage> {
        self.heaps.iter().find(|heap| heap.name == name)
    }
}

/// Projects `/mem` into [`MemoryStats`].
pub struct MemoryStatsProvider;

impl StatsProvider for MemoryStatsProvider {
    const PREFIX: &'static str = "/mem";
    type Report = MemoryStats;

    fn report<R: StatsReader>(reader: &R) -> Result<MemoryStats, Error> {
        let names = reader.names()?;
        let mut heaps = Vec::new();
        for name in names.iter().filter(|name| is_heap_entry(name)) {
            let heap_name = name
                .strip_prefix("/mem/")
                .expect("a heap entry starts with the family prefix")
                .to_owned();
            let value = reader.read(name)?;
            let MetricValue::Simple(rows) = value else {
                return Err(Error::UnexpectedMetricType {
                    name: name.clone(),
                    expected: "counter_vector_simple",
                    actual: metric_type_name(&value),
                });
            };
            let usage = heap_usage(name, heap_name, rows)?;
            heaps.push(usage);
        }
        Ok(MemoryStats { heaps })
    }
}

/// `/mem/<heap>` names carry the heap; `/mem/<heap>/<alias>` names do not.
fn is_heap_entry(name: &str) -> bool {
    name.strip_prefix("/mem/")
        .is_some_and(|remainder| !remainder.is_empty() && !remainder.contains('/'))
}

/// Maps one 1-row, 7-column reading into its heap value.
fn heap_usage(
    name: &str,
    heap_name: String,
    rows: Vec<Vec<u64>>,
) -> Result<MemoryHeapUsage, Error> {
    if rows.len() != 1 {
        return Err(Error::UnexpectedRowCount {
            name: name.to_owned(),
            expected: 1,
            actual: rows.len(),
        });
    }
    let mut row = rows.into_iter();
    let columns = row.next().expect("one row was checked");
    if columns.len() != 7 {
        return Err(Error::UnexpectedColumnCount {
            name: name.to_owned(),
            expected: 7,
            actual: columns.len(),
        });
    }
    Ok(MemoryHeapUsage {
        name: heap_name,
        total_bytes: columns[0],
        used_bytes: columns[1],
        free_bytes: columns[2],
        used_mmap_bytes: columns[3],
        max_allocated_bytes: columns[4],
        free_chunk_count: columns[5],
        releasable_bytes: columns[6],
    })
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
