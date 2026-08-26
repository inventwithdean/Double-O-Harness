use std::io::Read;

use anyhow::Result;
use scraper::{ElementRef, Html, Selector};
use wreq::{Client, header};

#[derive(Debug)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Parses the html and extracts results in WebSearchResult struct.
fn get_structured_web_search_result(raw_html: &str) -> Result<Vec<WebSearchResult>> {
    let document = Html::parse_document(&raw_html);
    let title_selector = Selector::parse("a.result-link").unwrap();
    let snippet_selector = Selector::parse("td.result-snippet").unwrap();

    let mut search_results: Vec<WebSearchResult> = Vec::new();

    let titles_and_links = document.select(&title_selector);
    let snippets = document.select(&snippet_selector);

    for (link_node, snippet_node) in titles_and_links.zip(snippets) {
        let url = link_node.value().attr("href").unwrap_or("").to_string();

        let title = link_node
            .text()
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        let snippet = snippet_node
            .text()
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        if !url.is_empty() && !url.starts_with("/") {
            search_results.push(WebSearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    Ok(search_results)
}

/// Decompresses brotli bytes to string
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
pub async fn web_search(query: &str, client: &Client) -> Result<String> {
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

    match get_structured_web_search_result(&raw_html) {
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

pub struct PageContent {
    pub title: String,
    pub description: String,
    pub url: String,
    pub text: String,
    pub image_urls: Vec<String>,
}

pub async fn scrape_url(url: &str, client: &Client) -> Result<String> {
    let resp = client
        .get(url)
        .header(header::ACCEPT_ENCODING, "br, gzip, deflate")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(format!(
            "HTTP {} while fetching {url}",
            resp.status().as_u16()
        ));
    }

    if let Some(ct) = resp.headers().get(header::CONTENT_TYPE) {
        if !ct.to_str().unwrap_or("").contains("text/html") {
            return Ok(format!("Not an HTML page."));
        }
    }

    let raw_bytes = resp.bytes().await?;
    let raw_html = decompress_br(&raw_bytes);
    let page = extract_page_text(&raw_html, url);

    const MAX_CHARS: usize = 20_000;
    let mut text_content = page.text;
    if text_content.chars().count() > MAX_CHARS {
        text_content = text_content.chars().take(MAX_CHARS).collect::<String>();
        text_content.push_str("...[content truncated]");
    }

    let images_str = if page.image_urls.is_empty() {
        "None".to_string()
    } else {
        page.image_urls.join("\n")
    };

    let out = format!(
        "Title: {}\nDescription: {}\nURL: {}\nImages:\n{}\n\n{}",
        page.title, page.description, page.url, images_str, text_content
    );

    Ok(out)
}

fn extract_page_text(raw_html: &str, page_url: &str) -> PageContent {
    let document = Html::parse_document(raw_html);
    let title = document
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|n| collapse(n.text().collect::<String>()))
        .unwrap_or_default();

    let description = document
        .select(
            &Selector::parse(r#"meta[name="description"], meta[property="og:description"]"#)
                .unwrap(),
        )
        .next()
        .and_then(|n| n.value().attr("content"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let block_selector =
        Selector::parse("h1, h2, h3, h4, h5, h6, p, li, pre, blockquote, td, dd, dt, figcaption")
            .unwrap();

    let img_selector = Selector::parse("img").unwrap();
    let base_url = url::Url::parse(page_url).ok();

    let mut lines: Vec<String> = Vec::new();
    let mut image_urls: Vec<String> = Vec::new();

    let mut extract_content = |root: scraper::ElementRef| {
        // Extract Text
        for node in root.select(&block_selector) {
            push_text(&mut lines, &node);
        }

        // Extract Images
        for node in root.select(&img_selector) {
            let src = node.value().attr("src").unwrap_or("").trim();
            let data_src = node.value().attr("data-src").unwrap_or("").trim();

            let raw_url = if !data_src.is_empty() && !data_src.starts_with("data:image/") {
                data_src
            } else if !src.is_empty() && !src.starts_with("data:image/") {
                src
            } else {
                ""
            };

            if !raw_url.is_empty() {
                // Resolve relative paths against base url
                let resolved = match &base_url {
                    Some(base) => base
                        .join(raw_url)
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| raw_url.to_string()),
                    None => raw_url.to_string(),
                };
                image_urls.push(resolved);
            }
        }
    };

    let mut found_containers = false;
    for sel_str in ["article", "main", "body"] {
        let selector = Selector::parse(sel_str).unwrap();
        let nodes: Vec<_> = document.select(&selector).collect();
        if !nodes.is_empty() {
            for node in nodes {
                extract_content(node);
            }
            found_containers = true;
            break;
        }
    }

    if !found_containers {
        extract_content(document.root_element());
    }

    image_urls.sort();
    image_urls.dedup();

    PageContent {
        title,
        description,
        url: page_url.to_string(),
        text: lines.join("\n"),
        image_urls,
    }
}

fn push_text(lines: &mut Vec<String>, node: &ElementRef) {
    let t = collapse(node.text().collect::<String>());
    if !t.is_empty() {
        lines.push(t);
    }
}

fn collapse(s: String) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
