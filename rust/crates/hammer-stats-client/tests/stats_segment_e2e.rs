#![cfg(target_os = "linux")]

//! End-to-end stats values: a real daemon, a real segment, a real client.
//!
//! Run with `HAMMER_DAEMON=/absolute/path/to/hammer cargo test -p
//! hammer-stats-client --test stats_segment_e2e -- --ignored`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hammer_stats_client::{
    BufferPoolStatsProvider, Error, MemoryStatsProvider, MetricValue, NodeCounter,
    NodeStatsProvider, StatsClient, SystemStatsProvider,
};

const DAEMON_BINARY: &str = "HAMMER_DAEMON";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(300);
/// One Data Worker: more than one hits an unrelated session worker-init race
/// in the daemon, which the per-column checks do not need.
const WORKER_COUNT: u64 = 1;
/// Worker count of the `/sys` vectors: one column per Data Worker.
const WORKER_COLUMNS: usize = WORKER_COUNT as usize;

struct HammerDaemon {
    child: Child,
    temp_dir: PathBuf,
    root_segment: PathBuf,
    api_segment: PathBuf,
    stats_socket: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    stopped: bool,
}

impl HammerDaemon {
    fn start(binary: &Path) -> Self {
        Self::start_with_config(binary, "")
    }

    /// Starts a daemon whose `[statseg]` section also carries `statseg_extra`,
    /// for example `per_node_counters = true`.
    fn start_with_config(binary: &Path, statseg_extra: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let prefix = format!("hammer-stats-e2e-{}-{unique}", std::process::id());
        let temp_dir = std::env::temp_dir().join(&prefix);
        fs::create_dir_all(&temp_dir).expect("E2E temporary directory is created");

        let config_path = temp_dir.join("startup.toml");
        let stdout_path = temp_dir.join("daemon.stdout");
        let stderr_path = temp_dir.join("daemon.stderr");
        let root_segment = PathBuf::from("/dev/shm").join(format!("{prefix}-global_vm"));
        let api_segment = PathBuf::from("/dev/shm").join(format!("{prefix}-vpe-api"));
        let stats_socket = temp_dir.join("stats.sock");
        fs::write(
            &config_path,
            format!(
                "plugins = []\n\n[memory]\nmain_heap_size = \"256 MiB\"\n\n[worker]\ncount = {WORKER_COUNT}\n\n[statseg]\nsocket_name = \"{}\"\nupdate_interval = \"50ms\"\n{statseg_extra}\n[api-segment]\nprefix = \"{prefix}\"\n",
                stats_socket.display()
            ),
        )
        .expect("E2E daemon configuration is written");

        let stdout = File::create(&stdout_path).expect("daemon stdout log is created");
        let stderr = File::create(&stderr_path).expect("daemon stderr log is created");
        let child = Command::new(binary)
            .arg(&config_path)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("configured Hammer daemon starts");

        Self {
            child,
            temp_dir,
            root_segment,
            api_segment,
            stats_socket,
            stdout_path,
            stderr_path,
            stopped: false,
        }
    }

    fn diagnostics(&mut self, error: impl std::fmt::Display) -> String {
        let status = self.child.try_wait();
        let stdout = fs::read_to_string(&self.stdout_path).unwrap_or_default();
        let stderr = fs::read_to_string(&self.stderr_path).unwrap_or_default();
        format!(
            "{error}\ndaemon status: {status:?}\ndaemon stdout:\n{stdout}\ndaemon stderr:\n{stderr}"
        )
    }

    /// Connects once the daemon publishes the stats listener.
    fn stats_client(&mut self) -> StatsClient {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            match StatsClient::connect(&self.stats_socket) {
                Ok(client) => return client,
                Err(error) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                    let _ = error;
                }
                Err(error) => panic!("{}", self.diagnostics(error)),
            }
        }
    }

    fn shutdown(&mut self) -> ExitStatus {
        if self.stopped {
            return self
                .child
                .try_wait()
                .expect("daemon status is readable")
                .expect("stopped daemon has exited");
        }
        let signal = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("kill command starts");
        assert!(signal.success(), "SIGTERM delivery succeeds: {signal:?}");

        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("daemon status is readable") {
                break status;
            }
            if Instant::now() >= deadline {
                self.child
                    .kill()
                    .expect("daemon is killed after shutdown timeout");
                break self.child.wait().expect("killed daemon is reaped");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        self.stopped = true;
        status
    }
}

impl Drop for HammerDaemon {
    fn drop(&mut self) {
        if !self.stopped && self.child.try_wait().is_ok_and(|status| status.is_none()) {
            if let Err(error) = self.child.kill() {
                eprintln!("failed to kill Hammer daemon during E2E cleanup: {error}");
            }
            if let Err(error) = self.child.wait() {
                eprintln!("failed to reap Hammer daemon during E2E cleanup: {error}");
            }
        }
        for path in [&self.api_segment, &self.root_segment] {
            if let Err(error) = fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("failed to remove E2E segment {}: {error}", path.display());
            }
        }
        if let Err(error) = fs::remove_dir_all(&self.temp_dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to remove E2E temporary directory: {error}");
        }
    }
}

/// Reads one heap's aliases and checks they equal the base columns.
fn assert_alias_matches_base(client: &StatsClient, heap: &str, column: usize, expected: u64) {
    let alias = format!("/mem/{heap}/{}", ["total", "used", "free"][column]);
    match client.read(&alias).expect("the alias is readable") {
        MetricValue::Simple(rows) => {
            assert_eq!(rows.len(), 1, "{alias} has one row");
            assert_eq!(rows[0].len(), 1, "{alias} is cropped to one column");
            assert_eq!(rows[0][0], expected, "{alias} selects the base column");
        }
        value => panic!("{alias} is a counter vector, got {value:?}"),
    }
}

#[test]
#[ignore = "requires HAMMER_DAEMON=/absolute/path/to/hammer"]
fn stats_values_are_published_and_readable() {
    let daemon_binary = std::env::var_os(DAEMON_BINARY)
        .map(PathBuf::from)
        .expect("HAMMER_DAEMON must name the Hammer daemon binary");
    let mut daemon = HammerDaemon::start(&daemon_binary);
    let client = daemon.stats_client();

    let names = client.names().expect("the directory is readable");
    assert!(
        names.iter().any(|name| name == "/mem/main heap"),
        "the main heap entry is published: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "/sys/num_worker_threads"),
        "the worker gauge is published: {names:?}"
    );

    let memory = match client.report::<MemoryStatsProvider>() {
        Ok(report) => report,
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    let mut heap_names: Vec<_> = memory.heaps.iter().map(|heap| heap.name.as_str()).collect();
    heap_names.sort_unstable();
    assert_eq!(
        heap_names,
        vec![
            "global_vm pvt",
            "main heap",
            "stat segment",
            "vpe-api data",
            "vpe-api pvt",
        ],
        "every heap owner registers exactly one entry"
    );
    for heap in &memory.heaps {
        assert!(heap.total_bytes > 0, "{} has a size", heap.name);
        assert!(heap.used_bytes > 0, "{} has allocations", heap.name);
        assert!(heap.free_bytes > 0, "{} has free space", heap.name);
        assert_eq!(
            heap.used_bytes + heap.free_bytes,
            heap.total_bytes + heap.used_mmap_bytes,
            "{}: used + free = total + mmapped, the dlmalloc mallinfo identity",
            heap.name
        );
        assert!(
            heap.max_allocated_bytes >= heap.used_bytes,
            "{}: the maximum footprint covers the current one",
            heap.name
        );
        assert!(
            heap.free_chunk_count > 0,
            "{} reports free chunks",
            heap.name
        );
    }
    let main_heap = memory.heap("main heap").expect("main heap is reported");
    assert!(
        main_heap.total_bytes <= 256 << 20,
        "the main heap stays inside its configured 256 MiB"
    );
    let stat_segment = memory
        .heap("stat segment")
        .expect("stat segment is reported");
    assert!(
        stat_segment.total_bytes <= 32 << 20,
        "the stats segment stays inside its configured 32 MiB"
    );

    assert_alias_matches_base(&client, "main heap", 0, main_heap.total_bytes);
    assert_alias_matches_base(&client, "main heap", 1, main_heap.used_bytes);
    assert_alias_matches_base(&client, "main heap", 2, main_heap.free_bytes);

    // The counters and the damped rate need at least one collect round and one
    // rate window, so read twice with a gap that covers several rounds.
    std::thread::sleep(Duration::from_secs(1));
    let first = match client.report::<SystemStatsProvider>() {
        Ok(report) => report,
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    std::thread::sleep(SAMPLE_INTERVAL);
    let second = match client.report::<SystemStatsProvider>() {
        Ok(report) => report,
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };

    assert_eq!(first.worker_thread_count, WORKER_COUNT);
    assert_eq!(first.main_loop_counts.len(), WORKER_COLUMNS);
    assert_eq!(first.loop_rates_per_second.len(), WORKER_COLUMNS);
    assert!(
        second.heartbeat > first.heartbeat,
        "the collect round advances the heartbeat: {} -> {}",
        first.heartbeat,
        second.heartbeat
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs();
    assert!(
        second.boottime_unix_seconds.abs_diff(now) < 300,
        "boottime is the daemon start time: {} vs {now}",
        second.boottime_unix_seconds
    );
    for (index, (earlier, later)) in first
        .main_loop_counts
        .iter()
        .zip(&second.main_loop_counts)
        .enumerate()
    {
        assert!(
            later >= earlier,
            "worker {index} count is cumulative: {earlier} -> {later}"
        );
        assert!(*later > 0, "worker {index} ran the main loop");
    }

    let derived = second
        .main_loop_rates_per_second(&first)
        .expect("two cumulative samples derive a rate");
    assert!(
        derived.iter().any(|rate| *rate > 0.0),
        "at least one worker ran loops in the sample window: {derived:?}"
    );
    for (index, (published, measured)) in second
        .loop_rates_per_second
        .iter()
        .zip(&derived)
        .enumerate()
    {
        if *measured == 0.0 {
            continue;
        }
        let published = *published as f64;
        assert!(
            published > 0.0,
            "worker {index} published a rate: {published}"
        );
        assert!(
            published > measured / 10.0 && published < measured * 10.0,
            "worker {index} published {published} loops/s, measured {measured} loops/s: \
             the rate is a current value, not an accumulating count"
        );
    }
    for (index, (earlier, later)) in first
        .loop_rates_per_second
        .iter()
        .zip(&second.loop_rates_per_second)
        .enumerate()
    {
        if *earlier == 0 || *later == 0 {
            continue;
        }
        assert!(
            *later < earlier.saturating_mul(10),
            "worker {index} rate does not accumulate across rounds: {earlier} -> {later}"
        );
    }

    let missing = client.read("/sys/not_a_metric");
    assert!(
        matches!(missing, Err(Error::MetricNotFound { .. })),
        "an unknown name is a typed error: {missing:?}"
    );

    // `/buffer-pools/<pool>/{cached,used,available}`: one Pool per Data Worker
    // NUMA node, three gauges each, and no bare `/buffer-pools/<pool>` entry.
    let first_pools = match client.report::<BufferPoolStatsProvider>() {
        Ok(report) => report,
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    assert_eq!(
        first_pools.pools.len(),
        1,
        "one Pool for the single Data Worker: {:?}",
        first_pools.pools
    );
    let pool = &first_pools.pools[0];
    assert!(
        pool.name.starts_with("default-numa-"),
        "a Pool is named after its NUMA node: {}",
        pool.name
    );
    assert!(
        pool.available > 0,
        "`{}` keeps buffers on its free list: {pool:?}",
        pool.name
    );
    assert!(
        pool.buffer_count() > 0,
        "`{}` published its three gauges: {pool:?}",
        pool.name
    );
    match client
        .read(&format!("/buffer-pools/{}/cached", pool.name))
        .expect("the cached gauge is readable")
    {
        MetricValue::Gauge(value) => {
            assert_eq!(value, pool.cached, "the report carries the gauge value");
        }
        value => panic!("`cached` is a gauge, got {value:?}"),
    }
    assert!(
        matches!(
            client.read(&format!("/buffer-pools/{}", pool.name)),
            Err(Error::MetricNotFound { .. })
        ),
        "the directory publishes no bare Pool entry"
    );

    std::thread::sleep(SAMPLE_INTERVAL);
    let second_pools = match client.report::<BufferPoolStatsProvider>() {
        Ok(report) => report,
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    assert_eq!(
        second_pools.pools.len(),
        1,
        "the Pool set is fixed at startup: {:?}",
        second_pools.pools
    );
    let second_pool = &second_pools.pools[0];
    assert_eq!(second_pool.name, pool.name, "a Pool keeps its identity");
    assert_eq!(
        second_pool.buffer_count(),
        pool.buffer_count(),
        "the three gauges keep describing one buffer count: {pool:?} then {second_pool:?}"
    );
    let unknown_pool = client.read("/buffer-pools/not-a-pool/cached");
    assert!(
        matches!(unknown_pool, Err(Error::MetricNotFound { .. })),
        "an unknown Pool is a typed error: {unknown_pool:?}"
    );

    // `/sys/node/*` exists only while the server's per-node switch is on; with
    // the switch off the family reads as absent, not as an error.
    match client.report::<NodeStatsProvider>() {
        Ok(report) => assert!(
            report.is_none(),
            "the node family is absent while the switch is off"
        ),
        Err(error) => panic!("an absent family is not an error: {error}"),
    }

    drop(client);
    let status = daemon.shutdown();
    assert!(
        status.success(),
        "{}",
        daemon.diagnostics(format!("daemon exits unsuccessfully: {status:?}"))
    );
}

/// Waits until the node-name vector and the counter shape are published
/// completely, then returns the node names by slot.
///
/// The shape is published before the names, and both before the first collect
/// round, so a client that attaches early sees a partly filled vector.
fn published_node_names(client: &StatsClient, daemon: &mut HammerDaemon) -> Vec<String> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        let complete = match (
            client.read("/sys/node/names"),
            client.read("/sys/node/calls"),
        ) {
            (Ok(MetricValue::Names(names)), Ok(MetricValue::Simple(rows))) => {
                !names.is_empty()
                    && names.iter().all(|name| !name.is_empty())
                    && rows.first().is_some_and(|row| row.len() == names.len())
            }
            _ => false,
        };
        if complete {
            return match client.read("/sys/node/names") {
                Ok(MetricValue::Names(names)) => names,
                value => panic!("`/sys/node/names` is a name vector, got {value:?}"),
            };
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            daemon.diagnostics("`/sys/node/names` is published with one name per node")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires HAMMER_DAEMON=/absolute/path/to/hammer"]
fn node_stats_are_published_and_readable() {
    let daemon_binary = std::env::var_os(DAEMON_BINARY)
        .map(PathBuf::from)
        .expect("HAMMER_DAEMON must name the Hammer daemon binary");
    let mut daemon = HammerDaemon::start_with_config(&daemon_binary, "per_node_counters = true\n");
    let client = daemon.stats_client();

    let node_names = published_node_names(&client, &mut daemon);
    let report = match client.report::<NodeStatsProvider>() {
        Ok(Some(report)) => report,
        Ok(None) => panic!(
            "{}",
            daemon.diagnostics("the node family is published while the switch is on")
        ),
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    assert_eq!(
        report.names, node_names,
        "the report carries the published names"
    );
    assert_eq!(
        report.thread_count() as u64,
        WORKER_COUNT + 1,
        "thread zero plus the Data Workers"
    );
    for counter in [
        NodeCounter::Clocks,
        NodeCounter::Vectors,
        NodeCounter::Calls,
        NodeCounter::Suspends,
    ] {
        let rows = report.counter(counter);
        assert_eq!(rows.len(), report.thread_count(), "{} rows", counter.name());
        assert!(
            rows.iter().all(|row| row.len() == node_names.len()),
            "`{}` has one column per node",
            counter.name()
        );
    }

    // Every node has four aliases, and each selects that node's own column of
    // the canonical entry it points at.
    for (slot, name) in node_names.iter().enumerate() {
        let node = report
            .node(name)
            .unwrap_or_else(|| panic!("`{name}` has one column"));
        for counter in [
            NodeCounter::Clocks,
            NodeCounter::Vectors,
            NodeCounter::Calls,
            NodeCounter::Suspends,
        ] {
            let alias = format!("/nodes/{name}/{}", counter.name());
            match client.read(&alias).expect("the alias is readable") {
                MetricValue::Simple(rows) => {
                    let selected: Vec<u64> = rows.iter().map(|row| row[0]).collect();
                    assert_eq!(
                        selected,
                        node.counter(counter),
                        "`{alias}` selects column {slot}"
                    );
                }
                value => panic!("`{alias}` is a cropped counter vector, got {value:?}"),
            }
        }
    }

    drop(client);
    let status = daemon.shutdown();
    assert!(
        status.success(),
        "{}",
        daemon.diagnostics(format!("daemon exits unsuccessfully: {status:?}"))
    );
}
