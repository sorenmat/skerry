//! Per-language-server LSP client.
//!
//! Spawns a child process (e.g. `rust-analyzer`), then runs two Tokio
//! tasks: one that reads framed JSON-RPC messages from the server's
//! stdout and one that writes framed messages to its stdin. The
//! synchronous `LspManager` talks to the client through channels.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::protocol::{decode_one, encode_message, Message};

/// Errors that can occur while managing a server process.
#[derive(Debug)]
pub enum LspError {
    Spawn(String),
    /// The server binary was not found on PATH (ENOENT) — surfaced
    /// separately so the editor can offer an install hint instead of a
    /// raw OS error string.
    SpawnNotFound,
    MissingPipe,
    Write(std::io::Error),
    Read(std::io::Error),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspError::Spawn(s) => write!(f, "failed to spawn language server: {s}"),
            LspError::SpawnNotFound => write!(f, "language server binary not found"),
            LspError::MissingPipe => write!(f, "stdio pipe missing from spawned server"),
            LspError::Write(e) => write!(f, "failed to write to server stdin: {e}"),
            LspError::Read(e) => write!(f, "failed to read from server stdout: {e}"),
        }
    }
}

impl std::error::Error for LspError {}

/// A handle to one running language server.
pub struct LspClient {
    outgoing: mpsc::UnboundedSender<Value>,
    #[allow(dead_code)]
    child: Child,
    next_id: Arc<AtomicU64>,
}

impl LspClient {
    /// Spawn `command[0]` with `command[1..]` as arguments and start
    /// the read/write tasks. Returns the client and a receiver for
    /// incoming messages from the server.
    pub async fn spawn(
        command: &[String],
    ) -> Result<(LspClient, mpsc::UnboundedReceiver<Message>), LspError> {
        if command.is_empty() {
            return Err(LspError::Spawn("empty command".to_string()));
        }

        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    LspError::SpawnNotFound
                } else {
                    LspError::Spawn(e.to_string())
                }
            })?;

        let stdin = child.stdin.take().ok_or(LspError::MissingPipe)?;
        let stdout = child.stdout.take().ok_or(LspError::MissingPipe)?;

        let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Message>();

        tokio::spawn(write_loop(stdin, out_rx));
        tokio::spawn(read_loop(stdout, in_tx));

        Ok((
            LspClient {
                outgoing: out_tx,
                child,
                next_id: Arc::new(AtomicU64::new(1)),
            },
            in_rx,
        ))
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn notify(&self, method: impl Into<String>, params: Value) {
        let msg = super::protocol::notification(method, params);
        let _ = self.outgoing.send(msg);
    }

    /// Send a JSON-RPC request and return its id so the caller can
    /// match the response later.
    pub fn request(&self, method: impl Into<String>, params: Value) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = super::protocol::request(id, method, params);
        let _ = self.outgoing.send(msg);
        id
    }

    /// Returns `true` if the child process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

async fn write_loop(
    mut stdin: tokio::process::ChildStdin,
    mut out_rx: mpsc::UnboundedReceiver<Value>,
) {
    while let Some(msg) = out_rx.recv().await {
        let bytes = encode_message(&msg);
        if let Err(e) = stdin.write_all(&bytes).await {
            eprintln!("LSP write error: {e}");
            break;
        }
        if let Err(e) = stdin.flush().await {
            eprintln!("LSP flush error: {e}");
            break;
        }
    }
}

async fn read_loop(mut stdout: tokio::process::ChildStdout, in_tx: mpsc::UnboundedSender<Message>) {
    let mut buf = Vec::with_capacity(4096);
    let mut read_buf = [0u8; 4096];

    loop {
        match stdout.read(&mut read_buf).await {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&read_buf[..n]),
            Err(e) => {
                eprintln!("LSP read error: {e}");
                break;
            }
        }

        while let Some(msg) = decode_one(&mut buf) {
            if in_tx.send(msg).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawning_missing_binary_reports_not_found() {
        let result = LspClient::spawn(&["skerry-no-such-binary-xyz".to_string()]).await;
        let err = match result {
            Ok(_) => panic!("spawning a missing binary must fail"),
            Err(e) => e,
        };
        assert!(
            matches!(err, LspError::SpawnNotFound),
            "expected SpawnNotFound, got {err:?}"
        );
        assert!(err.to_string().contains("not found"));
    }
}
