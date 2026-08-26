use anyhow::Result;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
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
use std::time::Duration;
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
    s3_client: aws_sdk_s3::Client,
    r2_bucket: String,
    tool_router: ToolRouter<WebIntelligence>,
}

#[tool_router]
impl WebIntelligence {
    pub fn new(
        http_client: Client,
        image_client: Client,
        s3_client: aws_sdk_s3::Client,
        r2_bucket: String,
    ) -> Self {
        Self {
            http_client,
            image_client,
            s3_client,
            r2_bucket,
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
        description = "Downloads the image on the MCP server (outside sandbox) and uploads on Cloudflare R2 from there, so you can use the direct r2 url via curl, as your sandbox only allows outbound network access to few essential sites, e.g. R2."
    )]
    async fn download_image(
        &self,
        Parameters(req): Parameters<DownloadImageRequest>,
    ) -> Result<CallToolResult, McpError> {
        let result = match download_and_upload_image(
            &req.url,
            &self.image_client,
            &self.s3_client,
            &self.r2_bucket,
        )
        .await
        {
            Ok(res) => res,
            Err(e) => format!("Image Download/Upload failed: {}", e),
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

    // Configure Cloudflare R2 Client
    let r2_endpoint = env::var("R2_ENDPOINT").expect("R2_ENDPOINT must be set");
    let r2_bucket = env::var("R2_BUCKET").expect("R2_BUCKET must be set");

    // Access Keys
    let access_key_id = env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID must be set");
    let access_key_secret =
        env::var("AWS_SECRET_ACCESS_KEY").expect("AWS_SECRET_ACCESS_KEY must be set");

    let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(r2_endpoint)
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            access_key_id,
            access_key_secret,
            None,
            None,
            "R2",
        ))
        .region("auto")
        .load()
        .await;

    let s3_client = aws_sdk_s3::Client::new(&sdk_config);

    // Download test
    // let url = download_and_upload_image(
    //     "https://static.wikia.nocookie.net/supernatural/images/6/6c/Who_We_Are_03.jpg/revision/latest?cb=20170512173320",
    //     &image_client,
    //     &s3_client,
    //     &r2_bucket,
    // ).await?;
    // println!("{url}");

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
        move || {
            Ok(WebIntelligence::new(
                client.clone(),
                image_client.clone(),
                s3_client.clone(),
                r2_bucket.clone(),
            ))
        },
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

async fn download_and_upload_image(
    url: &str,
    client: &Client,
    s3_client: &aws_sdk_s3::Client,
    bucket: &str,
) -> Result<String> {
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

    let content_type = &resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .unwrap_or("")
        .to_string();

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

    let raw_bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return Ok(format!("Failed to read image bytes: {}", e)),
    };

    let body = ByteStream::from(raw_bytes);

    match s3_client
        .put_object()
        .bucket(bucket)
        .key(&filename)
        .body(body)
        .content_type(content_type)
        .send()
        .await
    {
        Ok(_) => {}
        Err(e) => return Ok(format!("Failed to upload to R2: {}", e)),
    }

    let expires_in = Duration::from_secs(24 * 60 * 60);
    let presigning_config = match PresigningConfig::expires_in(expires_in) {
        Ok(config) => config,
        Err(e) => return Ok(format!("Failed to build presigning config: {}", e)),
    };

    let presigned_request = match s3_client
        .get_object()
        .bucket(bucket)
        .key(&filename)
        .presigned(presigning_config)
        .await
    {
        Ok(req) => req,
        Err(e) => return Ok(format!("Failed to generate presigned URL: {}", e)),
    };

    let final_url = presigned_request.uri().to_string();

    Ok(format!(
        "Image downloaded successfully!\nFilename: {}\nURL: {}",
        filename, final_url
    ))
}
