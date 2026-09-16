//! `/sys` projection: worker count, per-worker main-loop counters and rates,
//! and the fixed process scalars.

use std::time::Instant;

use hammer_stats_protocol::protocol::MetricValue;

use crate::error::Error;
use crate::provider::StatsProvider;
use crate::reader::StatsReader;

/// One process reading of the `/sys` family.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemStats {
    pub worker_thread_count: u64,
    /// `/sys/main_loop_count_per_worker`: the cumulative count, the authority
    /// for deriving rates over any window.
    pub main_loop_counts: Vec<u64>,
    /// `/sys/loops_per_worker`: the server's damped loops per second, a current
    /// value that must not be differenced.
    pub loop_rates_per_second: Vec<u64>,
    pub heartbeat: u64,
    pub boottime_unix_seconds: u64,
    /// Zero (no clear baseline yet) is mapped to `None`, so no caller subtracts
    /// a value the server has not published.
    pub last_stats_clear_unix_seconds: Option<u64>,
    /// When this report was produced, from the client's monotonic clock.
    pub sampled_at: Instant,
}

impl SystemStats {
    /// Derives the per-worker main-loop rate between two cumulative readings.
    ///
    /// Returns `None` when the two samples cannot be differenced: the earlier
    /// sample is not older, elapsed time is zero, the column counts differ, or
    /// a counter went backwards. The server's rate column is not involved.
    pub fn main_loop_rates_per_second(&self, earlier: &SystemStats) -> Option<Vec<f64>> {
        let elapsed = self
            .sampled_at
            .checked_duration_since(earlier.sampled_at)?
            .as_secs_f64();
        if elapsed <= 0.0 || self.main_loop_counts.len() != earlier.main_loop_counts.len() {
            return None;
        }
        let mut rates = Vec::with_capacity(self.main_loop_counts.len());
        for (current, previous) in self.main_loop_counts.iter().zip(&earlier.main_loop_counts) {
            let delta = current.checked_sub(*previous)?;
            rates.push(delta as f64 / elapsed);
        }
        Some(rates)
    }
}

/// Projects `/sys` into [`SystemStats`].
pub struct SystemStatsProvider;

impl StatsProvider for SystemStatsProvider {
    const PREFIX: &'static str = "/sys";
    type Report = SystemStats;

    fn report<R: StatsReader>(reader: &R) -> Result<SystemStats, Error> {
        let sampled_at = Instant::now();
        let worker_thread_count = read_u64(reader, "/sys/num_worker_threads")?;
        let main_loop_counts = read_row(reader, "/sys/main_loop_count_per_worker")?;
        let loop_rates_per_second = read_row(reader, "/sys/loops_per_worker")?;
        let worker_columns = usize::try_from(worker_thread_count).expect("u64 fits usize");
        for (name, columns) in [
            ("/sys/main_loop_count_per_worker", main_loop_counts.len()),
            ("/sys/loops_per_worker", loop_rates_per_second.len()),
        ] {
            if columns != worker_columns {
                return Err(Error::WorkerCountMismatch {
                    name: name.to_owned(),
                    worker_thread_count,
                    columns,
                });
            }
        }
        let last_stats_clear = read_u64(reader, "/sys/last_stats_clear")?;
        Ok(SystemStats {
            worker_thread_count,
            main_loop_counts,
            loop_rates_per_second,
            heartbeat: read_u64(reader, "/sys/heartbeat")?,
            boottime_unix_seconds: read_u64(reader, "/sys/boottime")?,
            last_stats_clear_unix_seconds: (last_stats_clear != 0).then_some(last_stats_clear),
            sampled_at,
        })
    }
}

/// One scalar entry as a `u64`.
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

/// The single row of one per-worker vector.
fn read_row<R: StatsReader>(reader: &R, name: &str) -> Result<Vec<u64>, Error> {
    match reader.read(name)? {
        MetricValue::Simple(rows) => {
            if rows.len() != 1 {
                return Err(Error::UnexpectedRowCount {
                    name: name.to_owned(),
                    expected: 1,
                    actual: rows.len(),
                });
            }
            Ok(rows.into_iter().next().expect("one row was checked"))
        }
        value => Err(Error::UnexpectedMetricType {
            name: name.to_owned(),
            expected: "counter_vector_simple",
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
