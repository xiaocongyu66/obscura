//! Search tool smoke: bing (global) must return structured results.

use obscura_embedder::tools::SearchEngine;
use obscura_embedder::HeadlessServo;
use std::time::Duration;

fn main() {
    let servo = HeadlessServo::new((1280, 800)).expect("boot");
    let results = servo
        .search(SearchEngine::Bing, "rust programming language", Duration::from_secs(45))
        .expect("bing search");
    println!("bing results: {}", results.len());
    for r in results.iter().take(3) {
        println!("  - {} | {}", r.title, r.url);
    }
    assert!(!results.is_empty(), "bing must return results");
    assert!(results.iter().any(|r| !r.url.is_empty()), "results need urls");
    println!("SEARCH OK — bing extraction live");
}
