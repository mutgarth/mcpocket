use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, anyhow};
use rmcp::transport::{AuthorizationManager, AuthorizationSession};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use url::Url;

use crate::config::{TransportConfig, load_config};
use crate::config_edit::set_server_bearer_token;

const AUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResult {
    pub server: String,
    pub scopes: Vec<String>,
}

pub async fn authenticate_http_server(
    config_path: &Path,
    server_name: &str,
) -> anyhow::Result<AuthResult> {
    let config = load_config(config_path)?;
    let server = config
        .servers
        .get(server_name)
        .ok_or_else(|| anyhow!("unknown MCP server \"{server_name}\""))?;
    let TransportConfig::Http { url, .. } = &server.transport else {
        anyhow::bail!("server \"{server_name}\" does not use HTTP auth");
    };

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind local OAuth callback")?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr()?.port()
    );

    let mut manager = AuthorizationManager::new(url.as_str()).await?;
    let metadata = manager.discover_metadata().await?;
    let scopes = metadata.scopes_supported.clone().unwrap_or_default();
    manager.set_metadata(metadata);
    let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
    let session =
        AuthorizationSession::new(manager, &scope_refs, &redirect_uri, Some("mcpocket"), None)
            .await?;

    open_auth_url(session.get_authorization_url())?;
    let callback = wait_for_callback(listener).await?;
    let token_response = session
        .handle_callback(&callback.code, &callback.state)
        .await
        .context("exchange OAuth code for token")?;
    let token = serde_json::to_value(&token_response)?
        .get("access_token")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("OAuth token response did not include access_token"))?;

    set_server_bearer_token(config_path, server_name, &token)?;

    Ok(AuthResult {
        server: server_name.to_owned(),
        scopes,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallbackQuery {
    code: String,
    state: String,
}

async fn wait_for_callback(listener: TcpListener) -> anyhow::Result<CallbackQuery> {
    let (mut stream, _) = timeout(AUTH_CALLBACK_TIMEOUT, listener.accept())
        .await
        .context("timed out waiting for OAuth callback")??;
    let mut buffer = [0_u8; 4096];
    let bytes = stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let query = parse_callback_request(&request)?;

    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nAuthentication complete. You can return to mcpocket.\n",
        )
        .await?;
    stream.shutdown().await?;
    Ok(query)
}

fn parse_callback_request(request: &str) -> anyhow::Result<CallbackQuery> {
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty OAuth callback request"))?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("OAuth callback request was missing path"))?;
    let url = Url::parse(&format!("http://127.0.0.1{path}"))?;
    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| anyhow!("OAuth callback did not include code"))?;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| anyhow!("OAuth callback did not include state"))?;
    Ok(CallbackQuery { code, state })
}

fn open_auth_url(url: &str) -> anyhow::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .with_context(|| format!("open OAuth URL: {url}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_oauth_callback_query() {
        let callback = parse_callback_request(
            "GET /callback?code=abc123&state=xyz HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .unwrap();

        assert_eq!(
            callback,
            CallbackQuery {
                code: "abc123".to_owned(),
                state: "xyz".to_owned()
            }
        );
    }
}
