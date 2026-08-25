use anyhow::Result;
use scraper::{Html, Selector};

#[derive(Debug)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Parses the html and extracts results in WebSearchResult struct.
pub fn get_structured_web_search_result(raw_html: &String) -> Result<Vec<WebSearchResult>> {
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
