//! obscura CLI — Servo-kernel headless browser entry point.
//!
//! Every subcommand rides the Servo kernel via `obscura-embedder`:
//! - `fetch`   rendered page dumps over the kernel (or raw HTTP via
//!             `obscura-net` for `--dump original` and batch mode)
//! - `scrape`  rendered parallel fetch, one kernel thread per URL
//! - `serve`   the CDP-compatible WS server over the kernel
//! - `mcp`     MCP server (stdio / WS / HTTP) over per-session kernels

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::builder::styling::AnsiColor;
use clap::builder::Styles;
use clap::{Parser, Subcommand};
use tokio::time::timeout;

use obscura_embedder::fetch::{fetch_rendered, FetchRequest};

mod original_fetch;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Yellow.on_default())
    .usage(AnsiColor::Yellow.on_default())
    .literal(AnsiColor::Green.on_default())
    .placeholder(AnsiColor::Cyan.on_default());

#[derive(Parser)]
#[command(
    name = "obscura",
    version = env!("OBSCURA_BUILD_VERSION"),
    about = "The open-source headless browser for AI agents and web scraping (Servo kernel).",
    styles = STYLES,
)]
struct Args {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct GlobalArgs {
    /// HTTP/SOCKS5 proxy for outgoing traffic (also via OBSCURA_PROXY).
    #[arg(long, global = true)]
    proxy: Option<String>,

    /// Override the User-Agent for raw HTTP fetches. Rendered fetches carry
    /// the kernel fingerprint's Chrome UA.
    #[arg(long, global = true)]
    user_agent: Option<String>,

    /// Use the stealth HTTP client (TLS fingerprint impersonation) for raw
    /// fetches. Rendered fetches always run the kernel's Chrome-fingerprint
    /// TLS stack.
    #[arg(long, global = true)]
    stealth: bool,

    /// Obey robots.txt: rendered fetches and scrapes pre-flight /robots.txt
    /// and refuse disallowed paths.
    #[arg(long, global = true)]
    obey_robots: bool,

    /// Kept for compatibility with scripts that pass it before `serve`;
    /// private-network gating applies to raw HTTP fetches via obscura-net.
    #[arg(long, global = true)]
    allow_private_network: bool,

    #[arg(long, global = true)]
    verbose: bool,

    #[arg(long, short, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch one URL (rendered through the kernel) or a batch of raw URLs.
    Fetch {
        url: Option<String>,

        /// Output format: html|text|links|markdown|original|assets|cookies.
        /// `original` streams the raw HTTP body verbatim (no rendering);
        /// `cookies` lists the page's cookies as JSON.
        #[arg(long)]
        dump: Option<DumpFormat>,

        /// Wait until this CSS selector exists before dumping (use with
        /// --eval to return a specific element's HTML).
        #[arg(long)]
        selector: Option<String>,

        /// Fixed post-load settle time in seconds (default: kernel waits
        /// for load Complete).
        #[arg(long)]
        wait: Option<u64>,

        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        #[arg(long, short)]
        eval: Option<String>,

        /// Write output to a file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,

        /// Read newline-delimited URLs and fetch each raw (batch mode is
        /// HTTP-only; use `scrape` for rendered output).
        #[arg(long)]
        file: Option<std::path::PathBuf>,

        /// Concurrent raw fetches in batch mode.
        #[arg(long, default_value_t = std::num::NonZeroUsize::new(1).unwrap())]
        concurrency: std::num::NonZeroUsize,

        /// Capture the settled page as a PNG.
        #[arg(long, short = 's', value_name = "FILE", conflicts_with = "file")]
        screenshot: Option<std::path::PathBuf>,
    },

    /// Render and extract many URLs concurrently (one kernel per URL).
    Scrape {
        urls: Vec<String>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, default_value_t = std::num::NonZeroUsize::new(10).unwrap())]
        concurrency: std::num::NonZeroUsize,

        /// Per-URL output inside the JSON lines: json|html|text|markdown.
        #[arg(long, default_value = "json")]
        format: String,

        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

    /// Run the CDP-compatible WebSocket server over the Servo kernel.
    Serve {
        #[arg(short, long, default_value_t = 9222)]
        port: u16,

        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },

    /// Run the MCP server (stdio by default; --http or --ws for transports).
    Mcp {
        #[arg(long)]
        http: bool,

        /// Run over WebSocket (each connection = isolated kernel session).
        #[arg(long)]
        ws: bool,

        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long, default_value_t = 3000)]
        port: u16,
    },
}

#[derive(Clone, Debug, clap::ValueEnum, PartialEq, Eq)]
enum DumpFormat {
    Html,
    Text,
    Links,
    Markdown,
    /// Stream the raw HTTP response body verbatim (binary-safe; no kernel).
    Original,
    /// Every sub-resource URL the rendered page references.
    Assets,
    /// The page's cookies as JSON (HttpOnly excluded, standard visibility).
    Cookies,
}

fn select_log_filter(verbose: bool, quiet: bool) -> &'static str {
    if verbose {
        "debug"
    } else if quiet {
        "off"
    } else {
        "warn"
    }
}

fn merge_proxy(global: Option<String>, command: Option<String>) -> Option<String> {
    command.or(global).filter(|s| !s.is_empty())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let g = &args.global;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(select_log_filter(g.verbose, g.quiet))
            }),
        )
        .init();

    let global_proxy = g
        .proxy
        .clone()
        .or_else(|| std::env::var("OBSCURA_PROXY").ok().filter(|s| !s.is_empty()));

    match &args.command {
        Command::Serve { port, host } => {
            print_banner(*port);
            if let Some(proxy) = &global_proxy {
                tracing::info!("Proxy: {proxy}");
            }
            // The kernel owns the fingerprint (UA pool + Chrome TLS); proxy
            // support lands with the kernel network stack's proxy plumbing.
            obscura_embedder::cdp_server::serve(host, *port, (1280, 800))
                .await
                .map_err(|e| anyhow::anyhow!("CDP server failed: {e}"))
        }
        Command::Mcp {
            http,
            ws,
            host,
            port,
        } => {
            if *ws {
                obscura_mcp::ws::run(host.clone(), *port, None, None, false).await
            } else if *http {
                obscura_mcp::http::run(host.clone(), *port, None, None, false).await
            } else {
                obscura_mcp::run().await
            }
        }
        Command::Fetch {
            url,
            dump,
            selector,
            wait,
            timeout: timeout_secs,
            eval,
            output,
            file,
            concurrency,
            screenshot,
        } => {
            run_fetch(
                url.clone(),
                dump.clone(),
                selector.clone(),
                *wait,
                *timeout_secs,
                eval.clone(),
                output.clone(),
                file.clone(),
                concurrency.get(),
                screenshot.clone(),
                global_proxy,
                g.stealth,
                g.user_agent.clone(),
                g.quiet,
                g.obey_robots,
            )
            .await
        }
        Command::Scrape {
            urls,
            eval,
            concurrency,
            format,
            timeout: timeout_secs,
        } => {
            run_parallel_scrape(
                urls.clone(),
                eval.clone(),
                concurrency.get(),
                format,
                *timeout_secs,
                g.quiet,
                g.obey_robots,
                global_proxy,
                g.user_agent.clone(),
            )
            .await
        }
    }
}

/// Pre-flight /robots.txt for a rendered fetch or scrape target. Mirrors the
/// self-engine's Page::navigate gate: fetch robots.txt once per origin,
/// parse, refuse disallowed paths with the same message the CLI tests assert.
async fn check_robots(
    url_str: &str,
    proxy: &Option<String>,
    user_agent: &Option<String>,
    timeout_secs: u64,
    cache: &obscura_net::RobotsCache,
) -> anyhow::Result<()> {
    let url = url::Url::parse(url_str)
        .map_err(|e| anyhow::anyhow!("Invalid URL '{}': {}", url_str, e))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Ok(());
    }
    let origin = url.origin().ascii_serialization();
    if !cache.contains(&origin) {
        let mut robots_url = url.clone();
        robots_url.set_path("/robots.txt");
        robots_url.set_query(None);
        robots_url.set_fragment(None);
        let resp = original_fetch::fetch_original_response(
            robots_url.as_str(),
            proxy.clone(),
            user_agent.clone(),
            timeout_secs,
            false,
        )
        .await;
        let body = match resp {
            Ok(resp) if resp.status == 200 => String::from_utf8_lossy(&resp.body).into_owned(),
            _ => String::new(),
        };
        let agent = user_agent.clone().unwrap_or_else(|| "obscura".into());
        cache.parse_and_store(&origin, &body, &agent);
    }
    if !cache.is_allowed(&origin, url.path()) {
        anyhow::bail!("Blocked by robots.txt: {url}");
    }
    Ok(())
}

fn print_banner(port: u16) {
    println!(
        r#"
   ____  _
  / __ \| |
 | |  | | |__  ___  ___ _   _ _ __ __ _
 | |  | | '_ \/ __|/ __| | | | '__/ _` |
 | |__| | |_) \__ \ (__| |_| | | | (_| |
  \____/| .__/|___/\___|\__,_|_|  \__,_|
        |_|
  Servo-kernel headless browser v{}
  CDP server: ws://127.0.0.1:{}/devtools/browser
"#,
        env!("OBSCURA_BUILD_VERSION"),
        port
    );
}

async fn run_fetch(
    url: Option<String>,
    dump: Option<DumpFormat>,
    selector: Option<String>,
    wait: Option<u64>,
    timeout_secs: u64,
    eval: Option<String>,
    output: Option<std::path::PathBuf>,
    file: Option<std::path::PathBuf>,
    concurrency: usize,
    screenshot: Option<std::path::PathBuf>,
    global_proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    quiet: bool,
    obey_robots: bool,
) -> anyhow::Result<()> {
    if let Some(file) = file {
        if url.is_some() {
            anyhow::bail!("Pass URLs via a positional argument or --file, not both.");
        }
        if screenshot.is_some() {
            anyhow::bail!("--screenshot is only supported for a single URL, not --file batch mode.");
        }
        match dump {
            None | Some(DumpFormat::Original) => {}
            Some(_) => anyhow::bail!(
                "batch mode (--file) only supports --dump original. Use `scrape` for rendered output."
            ),
        }
        let urls = read_urls_from_file(&file)?;
        original_fetch::run_batch_fetch(
            urls,
            concurrency,
            timeout_secs,
            user_agent,
            global_proxy,
            output,
            quiet,
            stealth,
        )
        .await?;
        return Ok(());
    }

    let url = url.ok_or_else(|| {
        anyhow::anyhow!("No URL provided. Pass a URL, or a list of URLs with --file <path>.")
    })?;
    let dump_specified = dump.is_some();
    let dump = dump.unwrap_or(DumpFormat::Html);

    if dump == DumpFormat::Original {
        let bytes = original_fetch::fetch_original_response(
            &url,
            global_proxy,
            user_agent,
            timeout_secs,
            stealth,
        )
        .await?
        .body;
        write_or_print_bytes(&bytes, output.as_ref()).await?;
        return Ok(());
    }

    if obey_robots {
        let cache = obscura_net::RobotsCache::new();
        check_robots(&url, &global_proxy, &user_agent, timeout_secs, &cache).await?;
    }

    if !quiet {
        eprintln!("Fetching {url}...");
    }

    let deadline = Duration::from_secs(timeout_secs.max(1));
    let rendered = timeout(deadline, {
        let url = url.clone();
        let eval = eval.clone();
        let selector = selector.clone();
        let shot = screenshot.is_some();
        tokio::task::spawn_blocking(move || {
            fetch_rendered(FetchRequest {
                url,
                timeout_secs: timeout_secs.max(1),
                screenshot: shot,
                eval,
                selector,
            })
        })
    })
    .await
    .map_err(|_| anyhow::anyhow!("Timed out fetching {url} after {timeout_secs}s"))?
    .map_err(|e| anyhow::anyhow!("join: {e}"))?
    .map_err(|e| anyhow::anyhow!("fetch failed: {e}"))?;

    if !quiet {
        eprintln!("Page loaded.");
    }

    // An explicit --wait is an extra settle pause after load Complete.
    if let Some(secs) = wait {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }

    // A bare --eval returns the eval value directly.
    if !dump_specified && selector.is_none() && screenshot.is_none() {
        if let Some(value) = rendered.eval_value {
            write_or_print(value, output.as_ref()).await?;
            return Ok(());
        }
    }

    if let Some(path) = screenshot {
        let png = rendered
            .screenshot_png
            .ok_or_else(|| anyhow::anyhow!("screenshot unavailable"))?;
        tokio::fs::write(&path, &png)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
        if !quiet {
            eprintln!("Screenshot saved to {}", path.display());
        }
        if !dump_specified && selector.is_none() {
            return Ok(());
        }
    }

    let rendered_body = match dump {
        DumpFormat::Html => rendered.html,
        DumpFormat::Text => rendered.text,
        DumpFormat::Links => rendered.links,
        DumpFormat::Markdown => rendered.markdown,
        DumpFormat::Assets => rendered.assets,
        DumpFormat::Cookies => rendered.cookies,
        DumpFormat::Original => unreachable!("handled before kernel launch"),
    };
    write_or_print(rendered_body, output.as_ref()).await?;
    Ok(())
}

async fn run_parallel_scrape(
    urls: Vec<String>,
    eval: Option<String>,
    concurrency: usize,
    format: &str,
    timeout_secs: u64,
    quiet: bool,
    obey_robots: bool,
    global_proxy: Option<String>,
    user_agent: Option<String>,
) -> anyhow::Result<()> {
    let total = urls.len();
    if total == 0 {
        anyhow::bail!("No URLs provided. Pass at least one URL to scrape.");
    }
    if !quiet {
        eprintln!(
            "Scraping {total} URLs with {concurrency} concurrent kernels (per-URL timeout: {timeout_secs}s)..."
        );
    }
    let start = Instant::now();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let eval = Arc::new(eval);
    let format = Arc::new(format.to_string());
    let robots_cache = Arc::new(obscura_net::RobotsCache::new());

    let mut handles = Vec::new();
    for url in urls {
        let sem = semaphore.clone();
        let eval = eval.clone();
        let format = format.clone();
        let robots_cache = robots_cache.clone();
        let global_proxy = global_proxy.clone();
        let user_agent = user_agent.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let task_start = Instant::now();
            if obey_robots {
                if let Err(e) = check_robots(
                    &url,
                    &global_proxy,
                    &user_agent,
                    timeout_secs,
                    &robots_cache,
                )
                .await
                {
                    return serde_json::json!({
                        "url": url,
                        "ok": false,
                        "error": e.to_string(),
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            }
            let result = tokio::task::spawn_blocking(move || {
                fetch_rendered(FetchRequest {
                    url: url.clone(),
                    timeout_secs,
                    screenshot: false,
                    eval: (*eval).clone(),
                    selector: None,
                })
            })
            .await
            .map_err(|e| format!("join: {e}"))
            .and_then(|inner| inner.map_err(|e| e));

            let time_ms = task_start.elapsed().as_millis();
            match result {
                Ok(out) => {
                    let body = match format.as_str() {
                        "html" => serde_json::json!({ "html": out.html }),
                        "text" => serde_json::json!({ "text": out.text }),
                        "markdown" => serde_json::json!({ "markdown": out.markdown }),
                        _ => serde_json::json!({
                            "html": out.html,
                            "text": out.text,
                        }),
                    };
                    serde_json::json!({
                        "url": url,
                        "ok": true,
                        "content": body,
                        "time_ms": time_ms,
                    })
                }
                Err(e) => serde_json::json!({
                    "url": url,
                    "ok": false,
                    "error": e,
                    "time_ms": time_ms,
                }),
            }
        }));
    }

    let mut out = String::new();
    for handle in handles {
        if let Ok(line) = handle.await {
            out.push_str(&serde_json::to_string(&line).unwrap_or_default());
            out.push('\n');
        }
    }
    print!("{out}");
    if !quiet {
        eprintln!("Done in {:.1}s", start.elapsed().as_secs_f64());
    }
    Ok(())
}

async fn write_or_print(content: String, output: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    match output {
        Some(path) => tokio::fs::write(path, content.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?,
        None => print!("{content}"),
    }
    Ok(())
}

async fn write_or_print_bytes(
    content: &[u8],
    output: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    match output {
        Some(path) => tokio::fs::write(path, content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?,
        None => {
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(content)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to write stdout: {e}"))?;
            stdout
                .flush()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to flush stdout: {e}"))?;
        }
    }
    Ok(())
}

/// Read newline-delimited URLs from `path` (or stdin when `path` is `-`).
fn read_urls_from_file(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let content = if path == std::path::Path::new("-") {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| anyhow::anyhow!("Failed to read URLs from stdin: {}", e))?;
        s
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
    };
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}
