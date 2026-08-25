use anyhow::Result;
use axum::http::header;
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
use tower_http::services::ServeDir;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wreq::{Client, Proxy};
use wreq_util::Emulation;

mod web;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScrapeRequest {
    pub url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadImageRequest {
    pub url: String,
}

#[derive(Clone)]
pub struct WebIntelligence {
    http_client: Client,
    image_client: Client,
    tool_router: ToolRouter<WebIntelligence>,
}

#[tool_router]
impl WebIntelligence {
    pub fn new(http_client: Client, image_client: Client) -> Self {
        Self {
            http_client,
            image_client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Searches the web for the specified query")]
    async fn web_search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.http_client.clone();
        let results = match web::web_search(&req.query, &client).await {
            Ok(res) => res,
            Err(e) => format!("Search failed: {}", e),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(results)]))
    }

    #[tool(description = "Scrapes the specified URL for content")]
    async fn scrape_url(
        &self,
        Parameters(req): Parameters<ScrapeRequest>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.http_client.clone();
        let result = match web::scrape_url(&req.url, &client).await {
            Ok(res) => res,
            Err(e) => format!("Scrape failed: {}", e),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
    }

    #[tool(
        description = "Downloads the image at a remote server (outside the sandbox) which can be accessed inside sandbox via a temporary direct link using curl/wget. Have to do it this way, because direct image downloads from the sandbox are mostly blocked."
    )]
    async fn download_image(
        &self,
        Parameters(req): Parameters<DownloadImageRequest>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.image_client.clone();
        let result = match download_image(&req.url, &client).await {
            Ok(res) => res,
            Err(e) => format!("Image Download failed: {}", e),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
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

    let image_client = Client::builder().emulation(Emulation::Safari26).build()?;

    // Make sure temporary folder exists
    tokio::fs::create_dir_all("./temp_assets")
        .await
        .unwrap_or(());

    // Mount the image folder on main HTTP port
    let asset_router = axum::Router::new().nest_service("/assets", ServeDir::new("temp_assets"));
    let asset_listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Public CDN running on port 80");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(asset_listener, asset_router).await {
            eprintln!("Asset server crashed: {}", e);
        }
    });

    // Add Tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".to_string().into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let ct = tokio_util::sync::CancellationToken::new();

    let service = StreamableHttpService::new(
        move || Ok(WebIntelligence::new(client.clone(), image_client.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(ct.child_token())
            .disable_allowed_hosts(),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    // Make it inaccessible from outside the server.
    let tcp_listener = tokio::net::TcpListener::bind("172.17.0.1:1337").await?;
    let _ = axum::serve(tcp_listener, router)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.unwrap();
            ct.cancel();
        })
        .await;

    Ok(())
}

async fn download_image(url: &str, client: &Client) -> Result<String> {
    let resp = match client
        .get(url)
        .header(
            header::ACCEPT,
            "image/avif,image/webp,image/png,image/svg+xml,image/*;q=0.8,*/*;q=0.5",
        )
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(format!("Get request failed: {}", e)),
    };

    if !resp.status().is_success() {
        return Ok(format!("Server returned HTTP {}", resp.status()));
    }

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .unwrap_or("");

    if !content_type.starts_with("image/") {
        let safe_ct = if content_type.is_empty() {
            "unknown"
        } else {
            content_type
        };
        return Ok(format!(
            "Image download failed: URL did not return an image. Content-Type was '{}'",
            safe_ct
        ));
    }

    let file_ext = content_type
        .trim_start_matches("image/")
        .split('+')
        .next()
        .unwrap_or("jpg");

    let raw_id = uuid::Uuid::new_v4().simple().to_string();
    let short_id = &raw_id[..16];

    let filename = format!("{}.{}", short_id, file_ext);
    let filepath = format!("./temp_assets/{}", filename);

    let raw_bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return Ok(format!("Failed to read image bytes: {}", e)),
    };
    if let Err(e) = tokio::fs::write(&filepath, raw_bytes).await {
        return Ok(format!("Failed to save file to disk: {}", e));
    }

    Ok(format!(
        "Image downloaded successfully!\nFilename: {}\nURL: https://api.emergent.show/assets/{}",
        filename, filename
    ))
}
