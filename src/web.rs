use std::io::Read;

use anyhow::Result;
use scraper::{Html, Selector};
use wreq::{Client, header};

#[derive(Debug)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Parses the html and extracts results in WebSearchResult struct.
fn get_structured_web_search_result(raw_html: &String) -> Result<Vec<WebSearchResult>> {
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
pub async fn web_search(query: String, client: &Client) -> Result<String> {
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
