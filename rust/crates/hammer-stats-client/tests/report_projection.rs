//! Family projections over an in-memory directory fixture.
//!
//! The fixture implements [`StatsReader`], so a projection is exercised without
//! a socket, a mapping, or a daemon; the daemon end-to-end run lives in
//! `stats_segment_e2e.rs`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hammer_stats_client::{
    BufferPoolStatsProvider, Error, MemoryStatsProvider, MetricValue, StatsProvider, StatsReader,
    SystemStats, SystemStatsProvider,
};

/// The family projections only need names and values, so the fixture is a map.
struct DirectoryFixture {
    entries: HashMap<String, MetricValue>,
}

impl DirectoryFixture {
    fn new(entries: impl IntoIterator<Item = (&'static str, MetricValue)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        }
    }
}

impl StatsReader for DirectoryFixture {
    fn names(&self) -> Result<Vec<String>, Error> {
        let mut names: Vec<_> = self.entries.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    fn read(&self, name: &str) -> Result<MetricValue, Error> {
        self.entries
            .get(name)
            .cloned()
            .ok_or_else(|| Error::MetricNotFound {
                name: name.to_owned(),
            })
    }
}

/// Two heaps, their aliases, and the system entries the provider reads.
fn populated_fixture() -> DirectoryFixture {
    DirectoryFixture::new([
        (
            "/mem/main heap",
            MetricValue::Simple(vec![vec![4096, 2048, 2048, 0, 3000, 5, 128]]),
        ),
        (
            "/mem/main heap/total",
            MetricValue::Simple(vec![vec![4096]]),
        ),
        ("/mem/main heap/used", MetricValue::Simple(vec![vec![2048]])),
        (
            "/mem/stat segment",
            MetricValue::Simple(vec![vec![1024, 512, 512, 64, 600, 3, 32]]),
        ),
        ("/sys/num_worker_threads", MetricValue::Gauge(2)),
        (
            "/sys/main_loop_count_per_worker",
            MetricValue::Simple(vec![vec![100, 250]]),
        ),
        (
            "/sys/loops_per_worker",
            MetricValue::Simple(vec![vec![1000, 2500]]),
        ),
        ("/sys/heartbeat", MetricValue::Scalar(7)),
        ("/sys/boottime", MetricValue::Scalar(1_700_000_000)),
        ("/sys/last_stats_clear", MetricValue::Scalar(0)),
    ])
}

#[test]
fn memory_report_maps_the_seven_columns_and_ignores_aliases() {
    let directory = populated_fixture();
    let report = MemoryStatsProvider::report(&directory).expect("fixture is well formed");
    assert_eq!(report.heaps.len(), 2, "aliases are not heaps");
    let main = report.heap("main heap").expect("main heap is reported");
    assert_eq!(main.total_bytes, 4096);
    assert_eq!(main.used_bytes, 2048);
    assert_eq!(main.free_bytes, 2048);
    assert_eq!(main.used_mmap_bytes, 0);
    assert_eq!(main.max_allocated_bytes, 3000);
    assert_eq!(main.free_chunk_count, 5);
    assert_eq!(main.releasable_bytes, 128);
    assert!(report.heap("main heap/total").is_none());
    let segment = report
        .heap("stat segment")
        .expect("stat segment is reported");
    assert_eq!(segment.used_mmap_bytes, 64);
}

#[test]
fn memory_report_rejects_a_column_count_other_than_seven() {
    let directory =
        DirectoryFixture::new([("/mem/main heap", MetricValue::Simple(vec![vec![1, 2, 3]]))]);
    let error = MemoryStatsProvider::report(&directory).expect_err("three columns are rejected");
    match error {
        Error::UnexpectedColumnCount {
            name,
            expected,
            actual,
        } => {
            assert_eq!(name, "/mem/main heap");
            assert_eq!(expected, 7);
            assert_eq!(actual, 3);
        }
        other => panic!("expected a column-count error, got {other}"),
    }
}

#[test]
fn system_report_reads_worker_vectors_and_process_scalars() {
    let directory = populated_fixture();
    let report = SystemStatsProvider::report(&directory).expect("fixture is well formed");
    assert_eq!(report.worker_thread_count, 2);
    assert_eq!(report.main_loop_counts, vec![100, 250]);
    assert_eq!(report.loop_rates_per_second, vec![1000, 2500]);
    assert_eq!(report.heartbeat, 7);
    assert_eq!(report.boottime_unix_seconds, 1_700_000_000);
    assert_eq!(report.last_stats_clear_unix_seconds, None);
}

#[test]
fn system_report_keeps_a_published_clear_baseline() {
    let directory = DirectoryFixture::new([
        ("/sys/num_worker_threads", MetricValue::Gauge(1)),
        (
            "/sys/main_loop_count_per_worker",
            MetricValue::Simple(vec![vec![1]]),
        ),
        ("/sys/loops_per_worker", MetricValue::Simple(vec![vec![10]])),
        ("/sys/heartbeat", MetricValue::Scalar(1)),
        ("/sys/boottime", MetricValue::Scalar(1_700_000_000)),
        ("/sys/last_stats_clear", MetricValue::Scalar(1_700_000_100)),
    ]);
    let report = SystemStatsProvider::report(&directory).expect("fixture is well formed");
    assert_eq!(report.last_stats_clear_unix_seconds, Some(1_700_000_100));
}

#[test]
fn system_report_rejects_a_worker_count_mismatch() {
    let directory = DirectoryFixture::new([
        ("/sys/num_worker_threads", MetricValue::Gauge(2)),
        (
            "/sys/main_loop_count_per_worker",
            MetricValue::Simple(vec![vec![100]]),
        ),
        (
            "/sys/loops_per_worker",
            MetricValue::Simple(vec![vec![1000]]),
        ),
        ("/sys/heartbeat", MetricValue::Scalar(1)),
        ("/sys/boottime", MetricValue::Scalar(1_700_000_000)),
        ("/sys/last_stats_clear", MetricValue::Scalar(0)),
    ]);
    let error = SystemStatsProvider::report(&directory).expect_err("one column is not two");
    match error {
        Error::WorkerCountMismatch {
            name,
            worker_thread_count,
            columns,
        } => {
            assert_eq!(name, "/sys/main_loop_count_per_worker");
            assert_eq!(worker_thread_count, 2);
            assert_eq!(columns, 1);
        }
        other => panic!("expected a worker-count error, got {other}"),
    }
}

#[test]
fn main_loop_rates_are_derived_from_cumulative_columns() {
    let sampled_at = Instant::now();
    let earlier = SystemStats {
        worker_thread_count: 2,
        main_loop_counts: vec![100, 1000],
        loop_rates_per_second: vec![1, 2],
        heartbeat: 1,
        boottime_unix_seconds: 1,
        last_stats_clear_unix_seconds: None,
        sampled_at,
    };
    let later = SystemStats {
        main_loop_counts: vec![300, 3000],
        heartbeat: 2,
        sampled_at: sampled_at + Duration::from_secs(2),
        ..earlier.clone()
    };
    let rates = later
        .main_loop_rates_per_second(&earlier)
        .expect("two cumulative samples derive a rate");
    assert_eq!(rates, vec![100.0, 1000.0]);

    let same_instant = SystemStats {
        sampled_at,
        ..later.clone()
    };
    assert_eq!(same_instant.main_loop_rates_per_second(&earlier), None);

    let shorter = SystemStats {
        main_loop_counts: vec![300],
        ..later.clone()
    };
    assert_eq!(shorter.main_loop_rates_per_second(&earlier), None);

    let backwards = SystemStats {
        main_loop_counts: vec![1, 3000],
        ..later.clone()
    };
    assert_eq!(backwards.main_loop_rates_per_second(&earlier), None);
}

/// Two Buffer Pools and their three gauges: the directory lists gauges only.
fn buffer_pool_fixture() -> DirectoryFixture {
    DirectoryFixture::new([
        (
            "/buffer-pools/default-numa-0/cached",
            MetricValue::Gauge(64),
        ),
        ("/buffer-pools/default-numa-0/used", MetricValue::Gauge(100)),
        (
            "/buffer-pools/default-numa-0/available",
            MetricValue::Gauge(3_932),
        ),
        ("/buffer-pools/default-numa-1/cached", MetricValue::Gauge(0)),
        ("/buffer-pools/default-numa-1/used", MetricValue::Gauge(0)),
        (
            "/buffer-pools/default-numa-1/available",
            MetricValue::Gauge(4_096),
        ),
    ])
}

#[test]
fn buffer_pool_report_maps_the_three_gauges_of_every_pool() {
    let directory = buffer_pool_fixture();
    let report = BufferPoolStatsProvider::report(&directory).expect("fixture is well formed");
    assert_eq!(report.pools.len(), 2, "one Pool per NUMA node");
    let first = report.pool("default-numa-0").expect("node 0 is reported");
    assert_eq!(first.cached, 64);
    assert_eq!(first.used, 100);
    assert_eq!(first.available, 3_932);
    assert_eq!(
        first.buffer_count(),
        4_096,
        "the three gauges add up to the Pool's buffer count"
    );
    let second = report.pool("default-numa-1").expect("node 1 is reported");
    assert_eq!(second.buffer_count(), 4_096);
    assert_eq!(
        report
            .pools
            .iter()
            .map(|pool| pool.name.as_str())
            .collect::<Vec<_>>(),
        ["default-numa-0", "default-numa-1"],
        "Pools are ordered by name"
    );
    assert!(report.pool("default-numa-2").is_none());
    assert!(report.pool("default-numa-0/cached").is_none());
}

#[test]
fn buffer_pool_report_rejects_a_missing_gauge() {
    let directory = DirectoryFixture::new([
        ("/buffer-pools/default-numa-0/cached", MetricValue::Gauge(1)),
        ("/buffer-pools/default-numa-0/used", MetricValue::Gauge(2)),
    ]);
    let error =
        BufferPoolStatsProvider::report(&directory).expect_err("a missing gauge is an error");
    match error {
        Error::MetricNotFound { name } => {
            assert_eq!(name, "/buffer-pools/default-numa-0/available");
        }
        other => panic!("expected a missing-entry error, got {other}"),
    }
}

#[test]
fn buffer_pool_report_rejects_a_gauge_that_is_not_a_gauge() {
    let directory = DirectoryFixture::new([
        (
            "/buffer-pools/default-numa-0/cached",
            MetricValue::Simple(vec![vec![1]]),
        ),
        ("/buffer-pools/default-numa-0/used", MetricValue::Gauge(2)),
        (
            "/buffer-pools/default-numa-0/available",
            MetricValue::Gauge(3),
        ),
    ]);
    let error =
        BufferPoolStatsProvider::report(&directory).expect_err("a counter vector is not a gauge");
    match error {
        Error::UnexpectedMetricType {
            name,
            expected,
            actual,
        } => {
            assert_eq!(name, "/buffer-pools/default-numa-0/cached");
            assert_eq!(expected, "gauge");
            assert_eq!(actual, "counter_vector_simple");
        }
        other => panic!("expected a metric-type error, got {other}"),
    }
}
