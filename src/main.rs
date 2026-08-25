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
use std::{env, io::Read};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wreq::{Client, Proxy, header};
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
        let results = match web_search(req.query, &client).await {
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

fn decompress_br(raw_bytes: &[u8]) -> String {
    let mut raw_html = String::new();
    let mut decompressor = brotli::Decompressor::new(raw_bytes, 4096);
    if let Err(e) = decompressor.read_to_string(&mut raw_html) {
        println!("Brotli decompression failed (maybe not compressed): {e}");
        raw_html = String::from_utf8_lossy(raw_bytes).to_string();
    }
    raw_html
}

/// Performs Web Search and returns them in formatted string
async fn web_search(query: String, client: &Client) -> Result<String> {
    // Brotli compressed DuckDuckGo Lite endpoint
    // ~15KB per search
    // 1GB proxy bandwidth gives is ~66,000 queries.
    // Giving us ~16,500 searches per dollar (Decodo's Residential Rotating IPs pay to go is $4/GB)
    // ~170 searches per INR.

    let resp = client
        .post("https://lite.duckduckgo.com/lite/")
        .header(header::ACCEPT_ENCODING, "br, gzip, deflate")
        .form(&[("q", query)])
        .send()
        .await?;

    let raw_bytes = resp.bytes().await?;
    let raw_html = decompress_br(&raw_bytes);

    // println!("{raw_html}");
    let mut results_string = String::new();

    match web::get_structured_web_search_result(&raw_html) {
        Ok(search_results) => {
            for result in search_results {
                results_string.push_str(&format!(
                    "Title: {}\nURL: {}\nSnippet: {}\n\n",
                    result.title, result.url, result.snippet
                ));
            }
        }
        Err(_) => {
            results_string.push_str("No Results found!");
        }
    };

    Ok(results_string)
}
