use serde_json::Value;

use crate::config::{ServerConfig, exposed_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyReason {
    Allowlist,
    ReadOnly,
    Denylist,
    Destructive,
    UnknownRisk,
    InvalidTool,
}

impl PolicyReason {
    pub fn decision(self) -> PolicyDecision {
        match self {
            PolicyReason::Allowlist | PolicyReason::ReadOnly => PolicyDecision::Allow,
            PolicyReason::Denylist
            | PolicyReason::Destructive
            | PolicyReason::UnknownRisk
            | PolicyReason::InvalidTool => PolicyDecision::Deny,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PolicyReason::Allowlist => "allowlist",
            PolicyReason::ReadOnly => "read-only",
            PolicyReason::Denylist => "denylist",
            PolicyReason::Destructive => "destructive",
            PolicyReason::UnknownRisk => "unknown-risk",
            PolicyReason::InvalidTool => "invalid-tool",
        }
    }
}

pub fn decide_tool(server: &ServerConfig, raw_tool: &Value) -> PolicyDecision {
    explain_tool(server, raw_tool).decision()
}

pub fn explain_tool(server: &ServerConfig, raw_tool: &Value) -> PolicyReason {
    let Some(tool_name) = raw_tool.get("name").and_then(Value::as_str) else {
        return PolicyReason::InvalidTool;
    };
    let exposed = exposed_name(&server.name, tool_name);

    if server
        .gateway
        .deny_tools
        .iter()
        .any(|item| item == &exposed)
    {
        return PolicyReason::Denylist;
    }
    if server
        .gateway
        .allow_tools
        .iter()
        .any(|item| item == &exposed)
    {
        return PolicyReason::Allowlist;
    }

    if is_destructive(raw_tool) {
        PolicyReason::Destructive
    } else if is_clearly_read_only(raw_tool) {
        PolicyReason::ReadOnly
    } else {
        PolicyReason::UnknownRisk
    }
}

fn is_clearly_read_only(raw_tool: &Value) -> bool {
    let Some(annotations) = raw_tool.get("annotations") else {
        return false;
    };
    let read_only = annotations
        .get("readOnlyHint")
        .or_else(|| annotations.get("readOnly"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let destructive = annotations
        .get("destructiveHint")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    read_only && !destructive
}

fn is_destructive(raw_tool: &Value) -> bool {
    raw_tool
        .get("annotations")
        .and_then(|annotations| annotations.get("destructiveHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::config::{GatewayConfig, ServerConfig, TransportConfig};

    fn server() -> ServerConfig {
        ServerConfig {
            name: "github".to_owned(),
            enabled: true,
            targets: BTreeMap::new(),
            transport: TransportConfig::Http {
                url: "https://example.test".to_owned(),
                headers: BTreeMap::new(),
            },
            gateway: GatewayConfig {
                enabled: true,
                allow_tools: Vec::new(),
                deny_tools: Vec::new(),
            },
        }
    }

    #[test]
    fn allows_read_only_tools_by_default() {
        let tool = json!({"name":"search","annotations":{"readOnlyHint":true}});
        assert_eq!(decide_tool(&server(), &tool), PolicyDecision::Allow);
    }

    #[test]
    fn denies_unknown_and_destructive_tools_by_default() {
        assert_eq!(
            decide_tool(&server(), &json!({"name":"create_issue"})),
            PolicyDecision::Deny
        );
        assert_eq!(
            decide_tool(
                &server(),
                &json!({"name":"delete","annotations":{"readOnlyHint":true,"destructiveHint":true}})
            ),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn allowlist_and_denylist_use_exposed_names() {
        let mut server = server();
        server.gateway.allow_tools = vec!["github__create_issue".to_owned()];
        assert_eq!(
            decide_tool(&server, &json!({"name":"create_issue"})),
            PolicyDecision::Allow
        );

        server.gateway.deny_tools = vec!["github__create_issue".to_owned()];
        assert_eq!(
            decide_tool(
                &server,
                &json!({"name":"create_issue","annotations":{"readOnlyHint":true}})
            ),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn explains_policy_reason() {
        assert_eq!(
            explain_tool(
                &server(),
                &json!({"name":"delete","annotations":{"destructiveHint":true}})
            ),
            PolicyReason::Destructive
        );
        assert_eq!(
            explain_tool(&server(), &json!({"name":"unknown"})),
            PolicyReason::UnknownRisk
        );
    }
}
