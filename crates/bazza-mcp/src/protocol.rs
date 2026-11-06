use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bazza_service_api::{
    fixture_read::FixtureReadReply, outputs::ErrorCode, tool_permission, LibraryPermission,
    ToolSchema, MAX_REQUEST_BYTES,
};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
        ToolAnnotations,
    },
    service::{QuitReason, RequestContext, ServerInitializeError},
    transport::{async_rw::AsyncRwTransport, stdio},
    ErrorData, RoleServer, ServerHandler, ServiceExt,
};
use serde_json::Value;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::projection::{self, FixtureReads, FixtureTool};

// Room for the JSON-RPC envelope in addition to the domain argument budget.
const MAX_STDIO_LINE_BYTES: usize = MAX_REQUEST_BYTES + 65_536;
const CALLS_PER_WINDOW: u32 = 40;
const RATE_WINDOW: Duration = Duration::from_secs(1);
const MAX_IN_FLIGHT: usize = 4;
const PROTOCOL_VERSIONS: &[ProtocolVersion] =
    &[ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2026_07_28];

struct FixtureServer {
    reads: FixtureReads,
    tools: Vec<Tool>,
    rate: Mutex<CallWindow>,
    in_flight: Semaphore,
}

struct CallWindow {
    start: Instant,
    calls: u32,
}

impl CallWindow {
    fn admit(&mut self, now: Instant) -> bool {
        if now.duration_since(self.start) >= RATE_WINDOW {
            self.start = now;
            self.calls = 0;
        }
        if self.calls == CALLS_PER_WINDOW {
            return false;
        }
        self.calls += 1;
        true
    }
}

pub(crate) async fn serve_stdio(reads: FixtureReads) -> Result<(), ()> {
    let tools = projection::tools()
        .into_iter()
        .map(tool_definition)
        .collect::<Result<Vec<_>, ()>>()?;
    let (input, output) = stdio();
    let transport =
        AsyncRwTransport::new_server(input, output).with_max_line_length(MAX_STDIO_LINE_BYTES);
    let server = FixtureServer {
        reads,
        tools,
        rate: Mutex::new(CallWindow {
            start: Instant::now(),
            calls: 0,
        }),
        in_flight: Semaphore::new(MAX_IN_FLIGHT),
    };
    let running = match server.serve(transport).await {
        Ok(running) => running,
        Err(ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(_) => return Err(()),
    };
    match running.waiting().await {
        Ok(QuitReason::Closed | QuitReason::Cancelled) => Ok(()),
        _ => Err(()),
    }
}

fn tool_definition(
    FixtureTool {
        schema,
        description,
    }: FixtureTool,
) -> Result<Tool, ()> {
    let ToolSchema {
        name,
        input_schema: Value::Object(input),
        output_schema: Value::Object(output),
    } = schema
    else {
        return Err(());
    };
    let permission = tool_permission(name).ok_or(())?;
    if permission.required != LibraryPermission::Read || permission.include_url_requires.is_some() {
        return Err(());
    }
    Ok(Tool::new(name, description, input)
        .with_raw_output_schema(Arc::new(output))
        .with_annotations(ToolAnnotations::new().read_only(true).open_world(false)))
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerConfig {
        let (name, instructions) = if self.reads.is_managed() {
            (
                "bazza-mcp",
                "Read meeting data from the Bazza library selected for this grant.",
            )
        } else {
            (
                "bazza-mcp-fixture",
                "Read synthetic Bazza meeting data from an explicit fixture library.",
            )
        };
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
            .with_server_info(Implementation::new(name, env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(PROTOCOL_VERSIONS)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(ErrorData::invalid_params("Unknown tool list cursor", None));
        }
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.tools.iter().any(|tool| tool.name == request.name) {
            return Err(ErrorData::invalid_params("Unknown Bazza tool", None));
        }
        if request.request_state.is_some() || request.input_responses.is_some() {
            return Err(ErrorData::invalid_params(
                "This tool accepts only a single request",
                None,
            ));
        }
        let admitted = self
            .rate
            .lock()
            .map_err(|_| ErrorData::internal_error("Bazza tool unavailable", None))?
            .admit(Instant::now());
        if !admitted {
            return Ok(rate_limited(self.reads.library_id()));
        }
        let Ok(_permit) = self.in_flight.try_acquire() else {
            return Ok(rate_limited(self.reads.library_id()));
        };
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let response = self
            .reads
            .call(request.name.as_ref(), arguments)
            .await
            .ok_or_else(|| ErrorData::internal_error("Bazza tool unavailable", None))?;
        Ok(tool_result(response).into())
    }
}

fn rate_limited(library_id: Uuid) -> CallToolResponse {
    CallToolResult::structured_error(
        FixtureReadReply::failure(library_id, ErrorCode::RateLimited).output,
    )
    .into()
}

fn tool_result(response: Value) -> CallToolResult {
    if response.get("error").is_some() {
        CallToolResult::structured_error(response)
    } else {
        CallToolResult::structured(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_uses_bounded_windows_and_the_public_failure_shape() {
        let start = Instant::now();
        let mut window = CallWindow { start, calls: 0 };
        for _ in 0..CALLS_PER_WINDOW {
            assert!(window.admit(start));
        }
        assert!(!window.admit(start + Duration::from_millis(999)));
        assert!(window.admit(start + RATE_WINDOW));

        let library_id = Uuid::new_v4();
        let CallToolResponse::Complete(result) = rate_limited(library_id) else {
            panic!("expected a complete tool execution failure");
        };
        assert_eq!(result.is_error, Some(true));
        let failure = result.structured_content.unwrap();
        bazza_service_api::validate_tool_output("bazza_get_status", &failure).unwrap();
        assert_eq!(failure["library_id"], library_id.to_string());
        assert_eq!(failure["error"]["code"], "RATE_LIMITED");
    }
}
