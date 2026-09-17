//! `/buffer-pools` projection: one entry per Buffer Pool, three gauges each.
//!
//! The gauges are the server's buffer family contract: `cached` (buffers held
//! by the Worker thread caches), `used` (buffers handed out), and `available`
//! (buffers on the Pool free list). Pools are named after the NUMA node whose
//! Data Worker they serve, such as `default-numa-0`.

use hammer_stats_protocol::protocol::MetricValue;

use crate::error::Error;
use crate::provider::StatsProvider;
use crate::reader::StatsReader;

/// One Buffer Pool as the server last sampled it.
///
/// The three gauges are three independent reads, exactly like the server's
/// sampler: they are not one atomic snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferPoolUsage {
    /// The Pool name without the `/buffer-pools/` prefix.
    pub name: String,
    pub cached: u64,
    pub used: u64,
    pub available: u64,
}

impl BufferPoolUsage {
    /// The Pool's buffer count: VPP publishes `used` as
    /// `n_buffers - n_avail - Σ n_cached` of the same reading, so the three
    /// gauges add up to the count the Pool was created with.
    pub fn buffer_count(&self) -> u64 {
        self.cached + self.used + self.available
    }
}

/// Every Buffer Pool the segment publishes, ordered by name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferPoolStats {
    pub pools: Vec<BufferPoolUsage>,
}

impl BufferPoolStats {
    /// Looks one Pool up by its name without the `/buffer-pools/` prefix.
    pub fn pool(&self, name: &str) -> Option<&BufferPoolUsage> {
        self.pools.iter().find(|pool| pool.name == name)
    }
}

/// Projects `/buffer-pools` into [`BufferPoolStats`].
pub struct BufferPoolStatsProvider;

impl StatsProvider for BufferPoolStatsProvider {
    const PREFIX: &'static str = "/buffer-pools";
    type Report = BufferPoolStats;

    fn report<R: StatsReader>(reader: &R) -> Result<BufferPoolStats, Error> {
        let names = reader.names()?;
        let mut pools: Vec<BufferPoolUsage> = Vec::new();
        // The directory publishes no bare `/buffer-pools/<pool>` entry, so the
        // Pool set is read off the gauge names themselves.
        for name in &names {
            let Some(pool_name) = pool_of_gauge_entry(name) else {
                continue;
            };
            if pools.iter().any(|pool| pool.name == pool_name) {
                continue;
            }
            pools.push(BufferPoolUsage {
                name: pool_name.to_owned(),
                cached: read_gauge(reader, &format!("/buffer-pools/{pool_name}/cached"))?,
                used: read_gauge(reader, &format!("/buffer-pools/{pool_name}/used"))?,
                available: read_gauge(reader, &format!("/buffer-pools/{pool_name}/available"))?,
            });
        }
        pools.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(BufferPoolStats { pools })
    }
}

/// The Pool name a `/buffer-pools/<pool>/<gauge>` entry belongs to.
///
/// A metric this family does not publish is not a Pool, and a Pool is not
/// invented from a name that carries something else after the gauge.
fn pool_of_gauge_entry(name: &str) -> Option<&str> {
    let (pool, gauge) = name.strip_prefix("/buffer-pools/")?.split_once('/')?;
    let published = matches!(gauge, "cached" | "used" | "available");
    (published && !pool.is_empty()).then_some(pool)
}

/// One gauge reading; a missing gauge is the server's defect, not a zero.
fn read_gauge<R: StatsReader>(reader: &R, name: &str) -> Result<u64, Error> {
    match reader.read(name)? {
        MetricValue::Gauge(value) => Ok(value),
        value => Err(Error::UnexpectedMetricType {
            name: name.to_owned(),
            expected: "gauge",
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
