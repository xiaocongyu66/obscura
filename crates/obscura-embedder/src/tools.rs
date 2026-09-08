//! Agent-facing tool semantics over the Servo kernel: navigate, evaluate,
//! click, screenshot, and structured web search (CN Bing / Baidu).

use serde_json::{json, Value};

use crate::HeadlessServo;

/// Supported engines for the search tool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SearchEngine {
    /// 中国必应(cn.bing.com)
    Bing,
    /// 百度(www.baidu.com)
    Baidu,
    /// Google(非中国区出口时可用)
    Google,
}

impl SearchEngine {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "bing" | "cn-bing" | "cn_bing" => Some(Self::Bing),
            "baidu" => Some(Self::Baidu),
            "google" => Some(Self::Google),
            _ => None,
        }
    }

    pub fn search_url(self, query: &str) -> String {
        let q = urlencoding_encode(query);
        match self {
            Self::Bing => format!("https://cn.bing.com/search?q={q}&ensearch=0"),
            Self::Baidu => format!("https://www.baidu.com/s?wd={q}&ie=utf-8"),
            Self::Google => format!("https://www.google.com/search?q={q}&num=10"),
        }
    }
}

/// Minimal percent-encoding for query strings (keeps CJK chars readable).
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            },
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// One search result.
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Per-engine extraction JS: returns a JSON array of {title, url, snippet}.
/// Both engines load results server-side (no JS needed), so plain DOM reads
/// work; guarded with try/catch because selectors differ between layouts.
fn extraction_js(engine: SearchEngine) -> &'static str {
    match engine {
        SearchEngine::Bing => r#"
(function(){
  var out = [];
  document.querySelectorAll('#b_results > li.b_algo').forEach(function(li){
    var a = li.querySelector('h2 a');
    if (!a) return;
    var sn = li.querySelector('.b_caption p, .b_algoSlug, p');
    out.push({title: (a.innerText||'').trim(), url: a.href||'', snippet: (sn?sn.innerText:'').trim()});
  });
  return JSON.stringify(out.slice(0, 10));
})()"#,
        SearchEngine::Baidu => r#"
(function(){
  var out = [];
  document.querySelectorAll('#content_left .result, #content_left .c-container').forEach(function(d){
    var a = d.querySelector('h3 a');
    if (!a) return;
    var sn = d.querySelector('.c-abstract, [class*=content-right], .c-span-last');
    out.push({title: (a.innerText||'').trim(), url: a.href||'', snippet: (sn?sn.innerText:'').trim()});
  });
  return JSON.stringify(out.slice(0, 10));
})()"#,
        SearchEngine::Google => r#"
(function(){
  var out = [];
  document.querySelectorAll('div.g, div[data-sokoban-container]').forEach(function(d){
    var a = d.querySelector('a[href^="http"]');
    var h = d.querySelector('h3');
    if (!a || !h) return;
    out.push({title: (h.innerText||'').trim(), url: a.href||'', snippet: (d.innerText||'').slice(0, 300)});
  });
  return JSON.stringify(out.slice(0, 10));
})()"#,
    }
}

impl HeadlessServo {
    /// Run a search: navigate to the engine's search URL, wait for load,
    /// extract structured results. `deadline` covers the whole flow.
    pub fn search(
        &self,
        engine: SearchEngine,
        query: &str,
        deadline: std::time::Duration,
    ) -> Result<Vec<SearchResult>, String> {
        let ok = self.navigate(&engine.search_url(query), deadline)?;
        if !ok {
            return Err(format!("search page load did not complete ({query})"));
        }
        // Result containers settle after load; give the page a beat.
        for _ in 0..20 {
            self.spin();
            self.render_frame();
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        let raw = self.evaluate_sync(extraction_js(engine), deadline)?;
        if raw == "[]" {
            return Err(format!(
                "extraction empty; page url={:?} title={:?} body={:?}",
                self.evaluate_sync("location.href", Duration::from_secs(10)).unwrap_or_default(),
                self.evaluate_sync("document.title", Duration::from_secs(10)).unwrap_or_default(),
                self.evaluate_sync("(document.body.innerText||'').slice(0,200)", Duration::from_secs(10)).unwrap_or_default(),
            ));
        }
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("extraction parse failed ({raw:.120}): {e}"))?;
        let mut out = Vec::new();
        if let Some(items) = parsed.as_array() {
            for it in items {
                out.push(SearchResult {
                    title: it["title"].as_str().unwrap_or_default().to_string(),
                    url: it["url"].as_str().unwrap_or_default().to_string(),
                    snippet: it["snippet"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
        Ok(out)
    }

    /// Search tool as JSON (MCP/CLI/HTTP shared shape).
    pub fn search_json(
        &self,
        engine_name: &str,
        query: &str,
        deadline: std::time::Duration,
    ) -> Result<Value, String> {
        let engine = SearchEngine::from_str(engine_name)
            .ok_or_else(|| format!("unknown engine '{engine_name}' (bing|baidu|google)"))?;
        let results = self.search(engine, query, deadline)?;
        Ok(json!({
            "engine": engine_name,
            "query": query,
            "count": results.len(),
            "results": results.iter().map(|r| json!({
                "title": r.title, "url": r.url, "snippet": r.snippet,
            })).collect::<Vec<_>>(),
        }))
    }
}
