use anyhow::Result;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeResult, MetaObject, ProtocolVersion,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler};
use rmcp::{handler::server::tool::ToolRouter, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use std::env;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wreq::{Client, Proxy};
use wreq_util::Emulation;

mod web;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub query: String,
}

#[derive(Clone)]
pub struct WebIntelligence {
    http_client: Client,
    tool_router: ToolRouter<WebIntelligence>,
}

#[tool_router]
impl WebIntelligence {
    pub fn new(http_client: Client) -> Self {
        Self {
            http_client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Searches the web for the specified query")]
    async fn web_search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.http_client.clone();
        let results = match web::web_search(req.query, &client).await {
            Ok(res) => res,
            Err(e) => format!("Search failed: {}", e),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(results)]))
    }
}

#[tool_handler(meta = MetaObject(rmcp::object!({"tool_meta_key": "tool_meta_value"})))]
impl ServerHandler for WebIntelligence {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions("This server provides web search. Tools: web_search".to_string())
    }

    async fn initialize(
        &self,
        _request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        if let Some(http_request_part) = context.extensions.get::<axum::http::request::Parts>() {
            let initialize_headers = &http_request_part.headers;
            let initialize_uri = &http_request_part.uri;
            tracing::info!(?initialize_headers, %initialize_uri, "initialize from http server");
        }
        Ok(self.get_info())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let client = match env::var("MI6_PROXY") {
        Ok(proxy_url) => {
            println!("Initializing MI6 Proxy.");
            Client::builder()
                .emulation(Emulation::Safari26)
                .proxy(Proxy::https(proxy_url)?)
                .pool_max_idle_per_host(0)
                .build()?
        }
        Err(_) => {
            println!("Proxy not found. Continuing without proxy.");
            Client::builder().emulation(Emulation::Safari26).build()?
        }
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let ct = tokio_util::sync::CancellationToken::new();

    let service = StreamableHttpService::new(
        move || Ok(WebIntelligence::new(client.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(ct.child_token())
            .disable_allowed_hosts(),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let tcp_listener = tokio::net::TcpListener::bind("172.17.0.1:1337").await?;
    let _ = axum::serve(tcp_listener, router)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.unwrap();
            ct.cancel();
        })
        .await;

    Ok(())
}
