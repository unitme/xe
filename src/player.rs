use std::{
    env, fs,
    path::PathBuf,
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::{Child, Command},
};

use crate::youtube::StreamUrl;

pub struct MpvPlayer {
    child: Child,
    ipc_path: PathBuf,
}

impl MpvPlayer {
    pub async fn spawn() -> Result<Self> {
        let ipc_path =
            env::temp_dir().join(format!("play-{}-{}.sock", std::process::id(), unix_now()));
        let child = Command::new("mpv")
            .arg("--no-video")
            .arg("--idle=yes")
            .arg("--force-window=no")
            .arg("--no-terminal")
            .arg("--really-quiet")
            .arg(format!("--input-ipc-server={}", ipc_path.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start mpv; install mpv to enable playback")?;

        for _ in 0..50 {
            if ipc_path.exists() {
                return Ok(Self { child, ipc_path });
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        bail!("mpv started but did not create its IPC socket");
    }

    pub async fn load(&self, stream: &StreamUrl, volume: u8, muted: bool) -> Result<()> {
        self.apply_stream_headers(stream).await?;
        self.send(json!({"command": ["loadfile", stream.url.as_str(), "replace"]}))
            .await?;
        self.set_volume(volume).await?;
        self.set_mute(muted).await?;
        self.set_pause(false).await
    }

    pub async fn set_pause(&self, paused: bool) -> Result<()> {
        self.send(json!({"command": ["set_property", "pause", paused]}))
            .await
    }

    pub async fn set_volume(&self, volume: u8) -> Result<()> {
        self.send(json!({"command": ["set_property", "volume", volume]}))
            .await
    }

    pub async fn set_mute(&self, muted: bool) -> Result<()> {
        self.send(json!({"command": ["set_property", "mute", muted]}))
            .await
    }

    pub async fn stop(&self) -> Result<()> {
        self.send(json!({"command": ["stop"]})).await
    }

    pub async fn is_idle(&self) -> Result<bool> {
        let response = self
            .send_with_response(json!({"command": ["get_property", "idle-active"]}))
            .await?;
        Ok(response
            .get("data")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    async fn send(&self, command: Value) -> Result<()> {
        let mut stream = UnixStream::connect(&self.ipc_path)
            .await
            .context("failed to connect to mpv IPC")?;
        let payload = serde_json::to_vec(&command).context("failed to encode mpv IPC command")?;
        stream.write_all(&payload).await?;
        stream.write_all(b"\n").await?;
        stream.shutdown().await?;
        Ok(())
    }

    async fn send_with_response(&self, command: Value) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.ipc_path)
            .await
            .context("failed to connect to mpv IPC")?;
        let payload = serde_json::to_vec(&command).context("failed to encode mpv IPC command")?;
        stream.write_all(&payload).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        serde_json::from_str(&line).context("mpv IPC response was not JSON")
    }

    async fn apply_stream_headers(&self, stream: &StreamUrl) -> Result<()> {
        self.send(json!({"command": ["set_property", "user-agent", stream.user_agent]}))
            .await?;
        self.send(json!({"command": ["set_property", "referrer", stream.referer]}))
            .await?;
        self.send(json!({"command": ["set_property", "http-header-fields", stream.headers()]}))
            .await
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = fs::remove_file(&self.ipc_path);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
