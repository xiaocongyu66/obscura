//! Session fingerprint: UA from fake_user_agent's hard-coded pool, with
//! sec-ch-ua/platform derived from the parsed Chrome version so the HTTP
//! layer and JS-visible navigator stay coherent.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UaProfile {
    pub user_agent: String,
    pub ch_ua: String,
    pub ch_ua_platform: String,
    pub ch_ua_mobile: String,
    /// TLS impersonation profile (bogdanfinn/tls-client naming)
    pub impersonate: String,
}

/// Chrome versions with matching TLS profiles (bogdanfinn/tls-client
/// supports 131/124/120/110).
const TLS_SUPPORTED_VERSIONS: &[u32] = &[131, 124, 120, 110];

/// Pick a Chrome UA from fake_user_agent and derive the coherent
/// client-hint + TLS profile. Returns None if the picked UA's Chrome
/// version has no matching TLS profile (retry for another).
pub fn random_profile() -> Option<UaProfile> {
    let ua = fake_user_agent::get_chrome_rua().to_string();
    from_ua(&ua)
}

/// Build a profile from an explicit UA string.
pub fn from_ua(ua: &str) -> Option<UaProfile> {
    // Chrome/<major>
    let version: u32 = ua
        .split("Chrome/")
        .nth(1)?
        .split('.')
        .next()?
        .parse()
        .ok()?;
    if !TLS_SUPPORTED_VERSIONS.contains(&version) {
        return None;
    }
    let platform = if ua.contains("Macintosh") {
        "\"macOS\""
    } else if ua.contains("Windows") {
        "\"Windows\""
    } else {
        "\"Linux\""
    };
    let mobile = "?0";
    // sec-ch-ua GREASE ordering follows the version's era.
    let ch_ua = match version {
        131..=u32::MAX => {
            "\"Google Chrome\";v=\"{v}\", \"Chromium\";v=\"{v}\", \"Not_A Brand\";v=\"24\""
                .replace("{v}", &version.to_string())
        },
        124..=130 => {
            "\"Chromium\";v=\"{v}\", \"Google Chrome\";v=\"{v}\", \"Not-A.Brand\";v=\"99\""
                .replace("{v}", &version.to_string())
        },
        _ => "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"{v}\", \"Google Chrome\";v=\"{v}\""
            .replace("{v}", &version.to_string()),
    };
    Some(UaProfile {
        user_agent: ua.into(),
        ch_ua,
        ch_ua_platform: platform.into(),
        ch_ua_mobile: mobile.into(),
        impersonate: format!("chrome{version}"),
    })
}

/// Explicit version+platform profile (deterministic sessions).
pub fn profile_for(version: u32, platform: &str) -> Option<UaProfile> {
    let host = match platform {
        "macOS" => "Macintosh; Intel Mac OS X 10_15_7",
        "Windows" => "Windows NT 10.0; Win64; x64",
        _ => "X11; Linux x86_64",
    };
    from_ua(&format!(
        "Mozilla/5.0 ({host}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{version}.0.0.0 Safari/537.36"
    ))
}
