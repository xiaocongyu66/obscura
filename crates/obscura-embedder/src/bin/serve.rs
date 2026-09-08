//! Standalone CDP server process: one kernel, one fingerprint profile.
//! Spawned per-session for full isolation (UA/TLS/cookies/state).

use obscura_embedder::cdp_server::serve;
use obscura_embedder::fingerprint;

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|a| a.parse().ok()).unwrap_or(9223);
    let profile_idx: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    let profile = fingerprint::profile_by_index(profile_idx);
    eprintln!(
        "servo cdp session: port={port} ua={} imp={}",
        profile.user_agent, profile.impersonate
    );
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async move {
        if let Err(e) = serve("127.0.0.1", port, (1280, 800)).await {
            eprintln!("cdp server failed: {e}");
            std::process::exit(1);
        }
    });
}
