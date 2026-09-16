#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hammer_binary_api_client::Client;
use hammer_binary_api_rpc::VpeService;

const DAEMON_BINARY: &str = "HAMMER_DAEMON";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct HammerDaemon {
    child: Child,
    temp_dir: PathBuf,
    root_segment: PathBuf,
    api_segment: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    stopped: bool,
}

impl HammerDaemon {
    fn start(binary: &Path) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let prefix = format!("hammer-client-e2e-{}-{unique}", std::process::id());
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
                "plugins = []\n\n[memory]\nmain_heap_size = \"256 MiB\"\n\n[worker]\ncount = 1\n\n[stats]\nsocket_path = \"{}\"\n\n[api-segment]\nprefix = \"{prefix}\"\n",
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
            stdout_path,
            stderr_path,
            stopped: false,
        }
    }

    fn api_segment(&self) -> &Path {
        &self.api_segment
    }

    fn diagnostics(&mut self, error: impl std::fmt::Display) -> String {
        let status = self.child.try_wait();
        let stdout = fs::read_to_string(&self.stdout_path).unwrap_or_default();
        let stderr = fs::read_to_string(&self.stderr_path).unwrap_or_default();
        format!(
            "{error}\ndaemon status: {status:?}\ndaemon stdout:\n{stdout}\ndaemon stderr:\n{stderr}"
        )
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
        if let Err(error) = fs::remove_file(&self.api_segment)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to remove E2E API segment: {error}");
        }
        if let Err(error) = fs::remove_file(&self.root_segment)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to remove E2E root segment: {error}");
        }
        if let Err(error) = fs::remove_dir_all(&self.temp_dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to remove E2E temporary directory: {error}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires HAMMER_DAEMON=/absolute/path/to/hammer"]
async fn v2_transport_calls_vpe_show_version_and_disconnects() {
    let daemon_binary = std::env::var_os(DAEMON_BINARY)
        .map(PathBuf::from)
        .expect("HAMMER_DAEMON must name the Hammer daemon binary");
    let mut daemon = HammerDaemon::start(&daemon_binary);

    let connected = tokio::time::timeout(
        CONNECT_TIMEOUT,
        Client::connect(
            "netsystem-client-e2e",
            daemon.api_segment(),
            NonZeroU32::new(64).expect("response queue capacity is non-zero"),
            true,
        ),
    )
    .await;
    let mut client = match connected {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => panic!("{}", daemon.diagnostics(error)),
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    assert!(client.client_index().is_some());

    let reply = match VpeService::new(&mut client).show_version().await {
        Ok(reply) => reply,
        Err(error) => panic!("{}", daemon.diagnostics(error)),
    };
    assert_eq!(reply.program.as_str().expect("program is UTF-8"), "vpe");
    assert!(!reply.version.as_str().expect("version is UTF-8").is_empty());
    assert!(
        !reply
            .build_directory
            .as_str()
            .expect("build directory is UTF-8")
            .is_empty()
    );

    if let Err(error) = client.disconnect().await {
        panic!("{}", daemon.diagnostics(error));
    }
    assert!(client.client_index().is_none());
    drop(client);

    let status = daemon.shutdown();
    assert!(
        status.success(),
        "{}",
        daemon.diagnostics(format!("daemon exits unsuccessfully: {status:?}"))
    );
}
