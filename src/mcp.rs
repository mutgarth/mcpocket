use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::Value;

use crate::router::GatewayRouter;

pub async fn serve_stdio(router: GatewayRouter) -> anyhow::Result<()> {
    let service = GatewayServer { router }.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[derive(Clone)]
struct GatewayServer {
    router: GatewayRouter,
}

impl ServerHandler for GatewayServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_06_18)
            .with_server_info(Implementation::new("mcpocket", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = Vec::new();
        for value in self.router.list_tools().await {
            tools.push(serde_json::from_value::<Tool>(value).map_err(internal_error)?);
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let arguments = request.arguments.map(Value::Object);
        let result = self
            .router
            .call_tool(&request.name, arguments)
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;

        match serde_json::from_value::<CallToolResult>(result.clone()) {
            Ok(result) => Ok(result),
            Err(_) => Ok(CallToolResult::success(vec![Content::text(
                result.to_string(),
            )])),
        }
    }
}

fn internal_error(error: serde_json::Error) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}
