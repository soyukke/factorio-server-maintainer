//! SteamCMD wrapper — see spec §6.1.

use anyhow::Context;
use std::io::Read;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Official Steam-hosted bootstrap archive (always the latest steamcmd.exe).
const STEAMCMD_BOOTSTRAP_URL: &str = "https://media.steampowered.com/installer/steamcmd.zip";

/// Parameters for a `+app_update <id> validate` run.
#[derive(Clone, Debug)]
pub struct SteamCmdJob {
    pub steamcmd_exe: PathBuf,
    pub install_dir: PathBuf,
    pub app_id: u32,
    pub username: Option<String>,
}

impl SteamCmdJob {
    /// Compose the argv exactly as documented in spec §6.1:
    /// `+force_install_dir <dir> +login anonymous +app_update <id> validate +quit`
    pub fn argv(&self) -> Vec<String> {
        let login = self
            .username
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("anonymous");
        vec![
            "+force_install_dir".into(),
            self.install_dir.display().to_string(),
            "+login".into(),
            login.to_string(),
            "+app_update".into(),
            self.app_id.to_string(),
            "validate".into(),
            "+quit".into(),
        ]
    }
}

/// Make sure `steamcmd_exe` exists. If not, download Steam's official
/// bootstrap archive and extract it into the parent directory. Subsequent
/// `+app_update` runs will self-update steamcmd itself.
///
/// Progress is reported through `lines` so the caller can pipe it into the
/// same channel the actual run uses.
pub async fn ensure_installed(
    steamcmd_exe: &Path,
    lines: mpsc::Sender<String>,
) -> anyhow::Result<()> {
    if steamcmd_exe.is_file() {
        return Ok(());
    }

    let parent = steamcmd_exe
        .parent()
        .context("steamcmd path has no parent directory")?
        .to_path_buf();

    let _ = lines
        .send(format!(
            "[steamcmd] not found at {}; downloading bootstrap from {}",
            steamcmd_exe.display(),
            STEAMCMD_BOOTSTRAP_URL
        ))
        .await;

    let bytes = tokio::task::spawn_blocking(|| -> anyhow::Result<Vec<u8>> {
        let resp = ureq::get(STEAMCMD_BOOTSTRAP_URL)
            .call()
            .context("HTTP GET steamcmd.zip")?;
        let mut body = Vec::new();
        resp.into_reader()
            .read_to_end(&mut body)
            .context("read steamcmd.zip body")?;
        Ok(body)
    })
    .await
    .context("download task join")??;

    let _ = lines
        .send(format!(
            "[steamcmd] downloaded {} bytes; extracting to {}",
            bytes.len(),
            parent.display()
        ))
        .await;

    let parent_for_extract = parent.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        std::fs::create_dir_all(&parent_for_extract)
            .with_context(|| format!("create {}", parent_for_extract.display()))?;
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).context("open steamcmd.zip")?;
        archive
            .extract(&parent_for_extract)
            .context("extract steamcmd.zip")?;
        Ok(())
    })
    .await
    .context("extract task join")??;

    if !steamcmd_exe.is_file() {
        anyhow::bail!(
            "steamcmd.zip extracted but {} still missing",
            steamcmd_exe.display()
        );
    }

    let _ = lines.send("[steamcmd] ready".into()).await;
    Ok(())
}

/// Spawn `steamcmd.exe` asynchronously and stream every stdout/stderr line
/// through `lines`. Returns the process exit code.
///
/// On Windows the child is spawned with `CREATE_NO_WINDOW` so a manager exe
/// without a console doesn't flash a console window when it runs.
pub async fn run(job: &SteamCmdJob, lines: mpsc::Sender<String>) -> anyhow::Result<i32> {
    if !job.steamcmd_exe.is_file() {
        anyhow::bail!("steamcmd not found at {}", job.steamcmd_exe.display());
    }

    let mut cmd = Command::new(&job.steamcmd_exe);
    cmd.args(job.argv());
    let interactive = job.username.is_some();
    if interactive {
        cmd.stdin(std::process::Stdio::inherit());
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());
    } else {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    }

    #[cfg(windows)]
    {
        if interactive {
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            cmd.creation_flags(CREATE_NEW_CONSOLE);
        } else {
            // Anonymous updates need no input. Account updates may need a
            // password or Steam Guard code, so leave their console visible.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
    }

    let mut child = cmd.spawn().context("spawn steamcmd")?;

    if interactive {
        let _ = lines
            .send("[steamcmd] interactive console opened for Steam login".into())
            .await;
        drop(lines);
        let status = child.wait().await.context("wait steamcmd")?;
        return Ok(status.code().unwrap_or(-1));
    }

    let stdout = child.stdout.take().context("steamcmd stdout missing")?;
    let stderr = child.stderr.take().context("steamcmd stderr missing")?;

    let lines_stdout = lines.clone();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if lines_stdout.send(line).await.is_err() {
                break;
            }
        }
    });

    let lines_stderr = lines.clone();
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if lines_stderr.send(format!("[stderr] {line}")).await.is_err() {
                break;
            }
        }
    });

    // Drop our copy of the sender. Once the two task-owned clones drop as
    // their tasks exit, the receiver sees Closed and the pump task on the
    // caller side returns naturally.
    drop(lines);

    let status = child.wait().await.context("wait steamcmd")?;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    Ok(status.code().unwrap_or(-1))
}
