use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::Value;
use tracing::{info, warn};

use crate::config::{PocketConfig, exposed_name, split_exposed_name};
use crate::policy::{PolicyDecision, PolicyReason, decide_tool, explain_tool};
use crate::upstream::{StatusRow, UpstreamHandle};

#[derive(Clone)]
pub struct GatewayRouter {
    upstreams: Arc<BTreeMap<String, Arc<UpstreamHandle>>>,
}

#[derive(Debug, Clone)]
pub struct ToolInspectServer {
    pub name: String,
    pub transport: &'static str,
    pub tools: Vec<ToolInspectRow>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolInspectRow {
    pub exposed_name: String,
    pub decision: PolicyDecision,
    pub reason: PolicyReason,
}

impl GatewayRouter {
    pub fn new(config: PocketConfig) -> anyhow::Result<Self> {
        let upstreams = config
            .active_gateway_servers()
            .map(|server| (server.name.clone(), UpstreamHandle::new(server.clone())))
            .collect();
        Ok(Self {
            upstreams: Arc::new(upstreams),
        })
    }

    pub async fn list_tools(&self) -> Vec<Value> {
        let mut exposed = Vec::new();
        for upstream in self.upstreams.values() {
            match upstream
                .list_tools_with_timeout(Duration::from_secs(5))
                .await
            {
                Ok(tools) => {
                    for tool in tools {
                        if decide_tool(upstream.config(), &tool) == PolicyDecision::Allow {
                            if let Some(rewritten) =
                                rewrite_tool_name(upstream.config().name.as_str(), tool)
                            {
                                exposed.push(rewritten);
                            }
                        }
                    }
                }
                Err(error) => {
                    warn!(
                        server = upstream.config().name,
                        error = ?error,
                        "failed to list upstream tools"
                    );
                }
            }
        }
        exposed.sort_by(|left, right| {
            left.get("name")
                .and_then(Value::as_str)
                .cmp(&right.get("name").and_then(Value::as_str))
        });
        exposed
    }

    pub async fn call_tool(
        &self,
        exposed_tool: &str,
        arguments: Option<Value>,
    ) -> anyhow::Result<Value> {
        let (server_name, upstream_tool) = split_exposed_name(exposed_tool)?;
        let upstream = self
            .upstreams
            .get(server_name)
            .with_context(|| format!("unknown upstream server \"{server_name}\""))?;

        self.ensure_tool_allowed(upstream, upstream_tool, exposed_tool)
            .await?;

        let started = Instant::now();
        let result = upstream.call_tool(upstream_tool, arguments).await;
        let status = if result.is_ok() { "ok" } else { "error" };
        info!(
            server = server_name,
            tool = exposed_tool,
            duration_ms = started.elapsed().as_millis(),
            status,
            "tool call finished"
        );
        result
    }

    pub async fn status(&self) -> Vec<StatusRow> {
        let mut rows = Vec::new();
        for upstream in self.upstreams.values() {
            rows.push(upstream.status().await);
        }
        rows
    }

    pub async fn inspect_tools(&self, filter: Option<&str>) -> Vec<ToolInspectServer> {
        let mut rows = Vec::new();
        for upstream in self.upstreams.values() {
            if let Some(filter) = filter
                && upstream.config().name != filter
            {
                continue;
            }

            match upstream.list_tools().await {
                Ok(tools) => {
                    let mut tool_rows = tools
                        .iter()
                        .filter_map(|tool| {
                            let raw_name = tool.get("name")?.as_str()?.to_owned();
                            let exposed_name = exposed_name(&upstream.config().name, &raw_name);
                            let reason = explain_tool(upstream.config(), tool);
                            Some(ToolInspectRow {
                                exposed_name,
                                decision: reason.decision(),
                                reason,
                            })
                        })
                        .collect::<Vec<_>>();
                    tool_rows.sort_by(|left, right| left.exposed_name.cmp(&right.exposed_name));
                    rows.push(ToolInspectServer {
                        name: upstream.config().name.clone(),
                        transport: upstream.config().transport_name(),
                        tools: tool_rows,
                        error: None,
                    });
                }
                Err(error) => rows.push(ToolInspectServer {
                    name: upstream.config().name.clone(),
                    transport: upstream.config().transport_name(),
                    tools: Vec::new(),
                    error: Some(format!("{error:?}")),
                }),
            }
        }
        rows
    }

    async fn ensure_tool_allowed(
        &self,
        upstream: &Arc<UpstreamHandle>,
        upstream_tool: &str,
        exposed_tool: &str,
    ) -> anyhow::Result<()> {
        if upstream
            .config()
            .gateway
            .deny_tools
            .iter()
            .any(|item| item == exposed_tool)
        {
            anyhow::bail!("tool \"{exposed_tool}\" is denied by gateway policy");
        }
        if upstream
            .config()
            .gateway
            .allow_tools
            .iter()
            .any(|item| item == exposed_tool)
        {
            return Ok(());
        }

        let tools = upstream.list_tools().await?;
        let Some(tool) = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(upstream_tool))
        else {
            anyhow::bail!("upstream tool \"{upstream_tool}\" was not found");
        };

        if decide_tool(upstream.config(), tool) == PolicyDecision::Allow {
            Ok(())
        } else {
            anyhow::bail!("tool \"{exposed_tool}\" is hidden by gateway policy");
        }
    }
}

fn rewrite_tool_name(server: &str, mut tool: Value) -> Option<Value> {
    let object = tool.as_object_mut()?;
    let raw_name = object.get("name")?.as_str()?.to_owned();
    object.insert(
        "name".to_owned(),
        Value::String(exposed_name(server, &raw_name)),
    );
    Some(tool)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rewrites_tool_name_with_server_prefix() {
        let tool =
            rewrite_tool_name("github", json!({"name":"search","description":"Search"})).unwrap();
        assert_eq!(tool["name"], "github__search");
        assert_eq!(tool["description"], "Search");
    }
}
