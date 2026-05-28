use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::telemetry::Event;

/// Extract the pid from a `serve-<pid>.sock` file name.
pub fn parse_serve_pid(file_name: &str) -> Option<u32> {
    file_name
        .strip_prefix("serve-")?
        .strip_suffix(".sock")?
        .parse()
        .ok()
}

/// List serve socket paths currently present in `run_dir`.
pub fn list_socket_paths(run_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().and_then(parse_serve_pid).is_some())
        .map(|e| e.path())
        .collect()
}

/// Connect to a socket. If the connection is refused (the serve process is gone
/// but left its socket file behind), unlink the file and return `None`.
pub async fn connect_or_reap(path: &Path) -> std::io::Result<Option<UnixStream>> {
    match UnixStream::connect(path).await {
        Ok(stream) => Ok(Some(stream)),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            let _ = std::fs::remove_file(path);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Connect, then forward newline-delimited `Event` frames to `tx` until the
/// connection closes. Malformed lines are skipped.
pub async fn stream_socket(path: PathBuf, tx: mpsc::Sender<Event>) -> std::io::Result<()> {
    let Some(stream) = connect_or_reap(&path).await? else {
        return Ok(());
    };
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(event) = serde_json::from_str::<Event>(&line)
            && tx.send(event).await.is_err()
        {
            break;
        }
    }
    Ok(())
}

/// Periodically scan `run_dir` and keep one streaming task per live socket.
/// New sockets are connected; tasks that finish (disconnect) free their slot so
/// a later rescan reconnects. Runs until `tx` is closed.
pub async fn spawn_connection_manager(run_dir: PathBuf, tx: mpsc::Sender<Event>) {
    let mut active: HashSet<PathBuf> = HashSet::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<PathBuf>();

    loop {
        // Drain finished connections so they can be retried on the next scan.
        while let Ok(path) = done_rx.try_recv() {
            active.remove(&path);
        }

        for path in list_socket_paths(&run_dir) {
            if active.contains(&path) {
                continue;
            }
            active.insert(path.clone());
            let task_tx = tx.clone();
            let task_done = done_tx.clone();
            let task_path = path.clone();
            tokio::spawn(async move {
                let _ = stream_socket(task_path.clone(), task_tx).await;
                let _ = task_done.send(task_path);
            });
        }

        if tx.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{CallStatus, Event};

    #[test]
    fn parses_pid_from_socket_name() {
        assert_eq!(parse_serve_pid("serve-4823.sock"), Some(4823));
        assert_eq!(parse_serve_pid("serve-.sock"), None);
        assert_eq!(parse_serve_pid("other.txt"), None);
    }

    #[tokio::test]
    async fn connect_or_reap_removes_stale_socket() {
        let dir = std::env::temp_dir().join(format!("mcpocket-reap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("serve-1.sock");

        // Bind then drop the listener: the file remains but nothing listens.
        {
            let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        }
        assert!(path.exists());

        let result = connect_or_reap(&path).await.unwrap();
        assert!(result.is_none());
        assert!(!path.exists(), "stale socket should be reaped");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stream_socket_forwards_decoded_frames() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("mcpocket-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("serve-2.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        let server_path = path.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let ev = sample();
            let mut line = serde_json::to_string(&ev).unwrap();
            line.push('\n');
            stream.write_all(line.as_bytes()).await.unwrap();
            // keep the connection open briefly
            let _ = server_path;
        });

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = stream_socket(path, tx).await;
        });

        let got = rx.recv().await.unwrap();
        assert_eq!(got, sample());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn manager_merges_events_from_two_sockets() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("mcpocket-mgr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for pid in [10u32, 11u32] {
            let path = dir.join(format!("serve-{pid}.sock"));
            let listener = UnixListener::bind(&path).unwrap();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let ev = Event::ToolCall {
                    ts: pid as u64,
                    pid,
                    client: "c".to_owned(),
                    server: "s".to_owned(),
                    tool: "s__t".to_owned(),
                    duration_ms: 1,
                    status: CallStatus::Ok,
                };
                let mut line = serde_json::to_string(&ev).unwrap();
                line.push('\n');
                stream.write_all(line.as_bytes()).await.unwrap();
                std::future::pending::<()>().await; // hold open
            });
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let run_dir = dir.clone();
        tokio::spawn(async move {
            spawn_connection_manager(run_dir, tx).await;
        });

        let mut pids = std::collections::HashSet::new();
        for _ in 0..2 {
            if let Event::ToolCall { pid, .. } = rx.recv().await.unwrap() {
                pids.insert(pid);
            }
        }
        assert_eq!(pids, [10, 11].into_iter().collect());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample() -> Event {
        Event::ToolCall {
            ts: 1,
            pid: 2,
            client: "c".to_owned(),
            server: "github".to_owned(),
            tool: "github__x".to_owned(),
            duration_ms: 5,
            status: CallStatus::Ok,
        }
    }
}
