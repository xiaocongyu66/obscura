//! Search tool smoke: bing (global) must return structured results.

use obscura_embedder::tools::SearchEngine;
use obscura_embedder::HeadlessServo;
use std::time::Duration;

fn main() {
    let servo = HeadlessServo::new((1280, 800)).expect("boot");
    let results = match servo.search(SearchEngine::Bing, "rust programming language", Duration::from_secs(45)) {
        Ok(r) => r,
        Err(e) => {
            println!("search failed: {e}");
            let title = servo.evaluate_sync("document.title", Duration::from_secs(10)).unwrap_or_default();
            let url = servo.evaluate_sync("location.href", Duration::from_secs(10)).unwrap_or_default();
            let probe = servo.evaluate_sync(
                "JSON.stringify({li: document.querySelectorAll('li.b_algo').length, body: (document.body.innerText||'').slice(0,200)})",
                Duration::from_secs(10),
            ).unwrap_or_default();
            println!("title={title}\nurl={url}\nprobe={probe}");
            panic!("bing search failed");
        },
    };
    println!("bing results: {}", results.len());
    for r in results.iter().take(3) {
        println!("  - {} | {}", r.title, r.url);
    }
    assert!(!results.is_empty(), "bing must return results");
    assert!(results.iter().any(|r| !r.url.is_empty()), "results need urls");
    println!("SEARCH OK — bing extraction live");
}
