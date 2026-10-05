use runtime::environment::LocalProcessEnvironment;
use runtime::{PiBackend, PiConfig};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub struct Fixture {
    pub workspace: PathBuf,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "switchboard-rpc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&workspace).expect("create RPC fixture workspace");
        std::fs::write(workspace.join("mode"), mode).expect("write RPC fixture mode");
        Self { workspace }
    }

    pub fn config(&self) -> PiConfig {
        let mut config = PiConfig::local(self.workspace.clone());
        config.binary = env!("CARGO_BIN_EXE_scripted-pi").into();
        config.shutdown_grace = Duration::from_millis(100);
        config.execution_timeout = Duration::from_secs(10);
        config
    }

    pub fn backend(&self) -> PiBackend {
        PiBackend::new(self.config(), LocalProcessEnvironment).expect("valid fixture backend")
    }

    pub async fn wait_file(&self, name: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.workspace.join(name).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("child handshake timed out");
    }

    pub fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.workspace.join(name)).expect("read fixture handshake")
    }

    pub async fn assert_reaped(&self) {
        self.wait_file("pid").await;
        let pid =
            rustix::process::Pid::from_raw(self.read("pid").parse().expect("numeric child PID"))
                .expect("valid child PID");
        tokio::time::timeout(Duration::from_secs(5), async {
            while rustix::process::test_kill_process(pid).is_ok() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("RPC child was leaked or not reaped");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}
