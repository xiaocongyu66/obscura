//! Raw-HTTP fetch surface for the CLI (no kernel involvement): `--dump
//! original` streaming and batch `--file` mode. Carried over from the
//! pre-Servo CLI; the HTTP client is obscura-net, which is independent of
//! the browser kernel.

use std::sync::Arc;
use std::time::Instant;

use tokio::time::timeout;

/// Fetch a URL over raw HTTP and return the response (status, headers, body).
pub(crate) async fn fetch_original_response(
    url_str: &str,
    proxy: Option<String>,
    user_agent: Option<String>,
    timeout_secs: u64,
    stealth: bool,
) -> anyhow::Result<obscura_net::Response> {
    let url = url::Url::parse(url_str)
        .map_err(|e| anyhow::anyhow!("Invalid URL '{}': {}", url_str, e))?;

    // Stealth routes through the wreq TLS-impersonation client; file:// has
    // no TLS handshake to impersonate and wreq only speaks http(s).
    if stealth && url.scheme() != "file" {
        #[cfg(feature = "stealth")]
        {
            let client = obscura_net::StealthHttpClient::with_proxy(
                Arc::new(obscura_net::CookieJar::new()),
                proxy.as_deref(),
            );
            return match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => anyhow::bail!("Failed to fetch {}: {}", url_str, e),
                Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
            };
        }
        #[cfg(not(feature = "stealth"))]
        {
            let _ = stealth;
        }
    }

    let client = obscura_net::ObscuraHttpClient::with_options(
        Arc::new(obscura_net::CookieJar::new()),
        proxy.as_deref(),
    );
    if let Some(ua) = user_agent {
        client.set_user_agent(&ua).await;
    }
    match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => anyhow::bail!("Failed to fetch {}: {}", url_str, e),
        Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
    }
}

/// Batch raw fetch: one JSON status line per URL, input order preserved.
pub(crate) async fn run_batch_fetch(
    urls: Vec<String>,
    concurrency: usize,
    timeout_secs: u64,
    user_agent: Option<String>,
    proxy: Option<String>,
    output: Option<std::path::PathBuf>,
    quiet: bool,
    stealth: bool,
) -> anyhow::Result<()> {
    let total = urls.len();
    if total == 0 {
        anyhow::bail!("No URLs to fetch (--file was empty).");
    }
    if !quiet {
        eprintln!(
            "Fetching {total} URLs with {concurrency} concurrent request(s) (per-fetch timeout: {timeout_secs}s)..."
        );
    }

    let start = Instant::now();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let user_agent = Arc::new(user_agent);
    let proxy = Arc::new(proxy);

    let mut handles = Vec::with_capacity(total);
    for (i, url) in urls.into_iter().enumerate() {
        let sem = semaphore.clone();
        let user_agent = user_agent.clone();
        let proxy = proxy.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let task_start = Instant::now();
            let result = fetch_original_response(
                &url,
                (*proxy).clone(),
                (*user_agent).clone(),
                timeout_secs,
                stealth,
            )
            .await;
            let elapsed_ms = task_start.elapsed().as_millis();
            let line = match result {
                Ok(resp) => serde_json::json!({
                    "url": url,
                    "ok": (200..400).contains(&resp.status),
                    "status": resp.status,
                    "content_type": resp.headers.get("content-type").cloned().unwrap_or_default(),
                    "bytes": resp.body.len(),
                    "elapsed_ms": elapsed_ms,
                }),
                Err(e) => serde_json::json!({
                    "url": url,
                    "ok": false,
                    "error": e.to_string(),
                    "elapsed_ms": elapsed_ms,
                }),
            };
            (i, line)
        }));
    }

    let mut results: Vec<Option<serde_json::Value>> = vec![None; total];
    let mut failures = 0usize;
    for handle in handles {
        if let Ok((i, line)) = handle.await {
            if !line["ok"].as_bool().unwrap_or(false) {
                failures += 1;
            }
            results[i] = Some(line);
        } else {
            failures += 1;
        }
    }

    let mut out = String::new();
    for line in results.into_iter().flatten() {
        out.push_str(&serde_json::to_string(&line).unwrap_or_default());
        out.push('\n');
    }

    if let Some(path) = output {
        tokio::fs::write(&path, out.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        print!("{out}");
    }
    if !quiet {
        eprintln!(
            "Done in {:.1}s ({failures} failure(s))",
            start.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

use std::time::Duration;
