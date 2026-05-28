use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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
        if let Ok(event) = serde_json::from_str::<Event>(&line) {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    }
    Ok(())
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
