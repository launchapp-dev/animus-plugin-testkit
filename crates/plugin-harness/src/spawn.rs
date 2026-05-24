use std::net::TcpListener as StdTcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tempfile::TempDir;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{sleep, Duration};

/// Live plugin process plus its stdio handles.
pub struct PluginRunner {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<tokio::process::ChildStdout>,
    #[allow(dead_code)]
    pub stderr: Option<tokio::process::ChildStderr>,
    #[allow(dead_code)]
    shim_dir: Option<TempDir>,
    /// Background mock-oai HTTP server kept alive for the duration of this
    /// plugin run. Dropping the `Child` terminates the server.
    #[allow(dead_code)]
    oai_server: Option<Child>,
}

impl PluginRunner {
    pub async fn launch(plugin_path: &Path) -> Result<Self> {
        Self::launch_with_scenario(plugin_path, None).await
    }

    pub async fn launch_with_scenario(
        plugin_path: &Path,
        mock_scenario: Option<&str>,
    ) -> Result<Self> {
        let mut cmd = Command::new(plugin_path);

        let workspace_root = workspace_root();
        let shim_dir = build_path_shims(&workspace_root)?;
        if let Some(scenario) = mock_scenario {
            cmd.env("MOCK_SCENARIO", scenario);
        }

        if let Some(dir) = &shim_dir {
            let new_path = match std::env::var_os("PATH") {
                Some(existing) => {
                    let mut paths = vec![dir.path().to_path_buf()];
                    paths.extend(std::env::split_paths(&existing));
                    std::env::join_paths(paths).context("join PATH entries for plugin child")?
                }
                None => std::env::join_paths([dir.path()])
                    .context("join PATH entries for plugin child")?,
            };
            cmd.env("PATH", new_path);
        }

        if let Some(p) = mock_binary(&workspace_root, "mock-claude") {
            cmd.env("CLAUDE_BIN", p);
        }
        if let Some(p) = mock_binary(&workspace_root, "mock-codex") {
            cmd.env("CODEX_BIN", p);
        }
        if let Some(p) = mock_binary(&workspace_root, "mock-gemini") {
            cmd.env("GEMINI_BIN", p);
        }
        if let Some(p) = mock_binary(&workspace_root, "mock-opencode") {
            cmd.env("OPENCODE_BIN", p);
        }
        cmd.env("ANIMUS_TESTKIT", "1");

        // Stand up the mock-oai HTTP server on a free port so the oai plugin
        // can be exercised without real OPENAI_API_KEY credentials.
        let oai_server = if let Some(oai_bin) = mock_binary(&workspace_root, "mock-oai") {
            let port = find_free_port()?;
            let mut server = Command::new(&oai_bin);
            server
                .env("MOCK_OAI_PORT", port.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(scenario) = mock_scenario {
                server.env("MOCK_SCENARIO", scenario);
            }
            let child = server
                .spawn()
                .with_context(|| format!("spawn mock-oai from {}", oai_bin.display()))?;
            // Give axum a moment to bind before the plugin tries to connect.
            wait_for_port(port).await;
            cmd.env("OPENAI_API_KEY", "mock-oai-test-key");
            cmd.env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}/v1"));
            Some(child)
        } else {
            None
        };

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning plugin {}", plugin_path.display()))?;
        let stdin = child.stdin.take().context("plugin stdin missing")?;
        let stdout = BufReader::new(child.stdout.take().context("plugin stdout missing")?);
        let stderr = child.stderr.take();

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            shim_dir,
            oai_server,
        })
    }

    pub async fn shutdown(mut self) {
        let _ = self.stdin.shutdown().await;
        drop(self.stdin);
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(750), self.child.wait()).await;
        let _ = self.child.kill().await;
        if let Some(mut server) = self.oai_server.take() {
            let _ = server.kill().await;
        }
    }
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or(manifest)
}

fn mock_binary(workspace_root: &Path, name: &str) -> Option<PathBuf> {
    let candidates = [
        workspace_root.join("target/release").join(name),
        workspace_root.join("target/debug").join(name),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn find_free_port() -> Result<u16> {
    let listener = StdTcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

async fn wait_for_port(port: u16) {
    for _ in 0..40 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        sleep(Duration::from_millis(25)).await;
    }
}

/// Build a temp directory containing copies of the mock binaries named
/// `claude`, `codex`, `gemini`, `opencode` so plugins that spawn the literal
/// CLI name (and ignore env overrides) still get the mock.
///
/// We copy rather than symlink so chmod survives macOS quarantine semantics.
fn build_path_shims(workspace_root: &Path) -> Result<Option<TempDir>> {
    use std::fs;

    let pairs: Vec<(&str, &str)> = vec![
        ("mock-claude", "claude"),
        ("mock-codex", "codex"),
        ("mock-gemini", "gemini"),
        ("mock-opencode", "opencode"),
    ];

    let resolved: Vec<(PathBuf, &str)> = pairs
        .into_iter()
        .filter_map(|(src, dst)| mock_binary(workspace_root, src).map(|p| (p, dst)))
        .collect();

    if resolved.is_empty() {
        return Ok(None);
    }

    let tmp = tempfile::Builder::new()
        .prefix("animus-testkit-shims-")
        .tempdir()
        .context("create shim dir")?;
    for (src, dst) in resolved {
        let dst_path = tmp.path().join(dst);
        fs::copy(&src, &dst_path)
            .with_context(|| format!("copy {} → {}", src.display(), dst_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dst_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dst_path, perms)?;
        }
    }
    Ok(Some(tmp))
}
