//! Per-session browser fingerprint profiles, ported from the Go engine's
//! UAProfile pool (grok-free-register/pkg/engine/useragent.go). Each session
//! gets a coherent set: User-Agent + sec-ch-ua + platform, all matching the
//! same Chrome version so anti-bot layers can't find a mismatch.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UaProfile {
    /// User-Agent string
    pub user_agent: String,
    /// sec-ch-ua header value
    pub ch_ua: String,
    /// sec-ch-ua-platform value
    pub ch_ua_platform: String,
    /// sec-ch-ua-mobile value
    pub ch_ua_mobile: String,
    /// TLS impersonation profile name (bogdanfinn/tls-client compatible)
    pub impersonate: String,
}

/// 4 Chrome versions × 3 platforms = 12 coherent profiles.
pub const UA_PROFILES: &[UaProfile] = &[
    // === Chrome 131 ===
    UaProfile {
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\"".into(),
        ch_ua_platform: "\"Linux\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome131".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\"".into(),
        ch_ua_platform: "\"macOS\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome131".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\"".into(),
        ch_ua_platform: "\"Windows\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome131".into(),
    },
    // === Chrome 124 ===
    UaProfile {
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Chromium\";v=\"124\", \"Google Chrome\";v=\"124\", \"Not-A.Brand\";v=\"99\"".into(),
        ch_ua_platform: "\"Linux\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome124".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Chromium\";v=\"124\", \"Google Chrome\";v=\"124\", \"Not-A.Brand\";v=\"99\"".into(),
        ch_ua_platform: "\"macOS\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome124".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Chromium\";v=\"124\", \"Google Chrome\";v=\"124\", \"Not-A.Brand\";v=\"99\"".into(),
        ch_ua_platform: "\"Windows\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome124".into(),
    },
    // === Chrome 120 ===
    UaProfile {
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\"".into(),
        ch_ua_platform: "\"Linux\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome120".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\"".into(),
        ch_ua_platform: "\"macOS\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome120".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\"".into(),
        ch_ua_platform: "\"Windows\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome120".into(),
    },
    // === Chrome 110 ===
    UaProfile {
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"110\", \"Google Chrome\";v=\"110\"".into(),
        ch_ua_platform: "\"Linux\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome110".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"110\", \"Google Chrome\";v=\"110\"".into(),
        ch_ua_platform: "\"macOS\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome110".into(),
    },
    UaProfile {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36".into(),
        ch_ua: "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"110\", \"Google Chrome\";v=\"110\"".into(),
        ch_ua_platform: "\"Windows\"".into(),
        ch_ua_mobile: "?0".into(),
        impersonate: "chrome110".into(),
    },
];

/// Deterministic pick for reproducible sessions; use a random index for
/// fresh sessions.
pub fn profile_by_index(index: usize) -> &'static UaProfile {
    &UA_PROFILES[index % UA_PROFILES.len()]
}

/// Random profile (per-process RNG; sessions wanting entropy seed this
/// differently per session id).
pub fn random_profile() -> &'static UaProfile {
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        ^ (std::process::id() as usize);
    profile_by_index(idx)
}
