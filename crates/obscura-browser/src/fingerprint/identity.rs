//! `Fingerprint` — the per-session identity bundle.
//!
//! A `Fingerprint` is everything that should stay consistent for the lifetime
//! of one browsing session: UA, screen, timezone, language, platform, hardware
//! concurrency, device memory, WebGL vendor/renderer, font list, and a seed for
//! the deterministic NoiseEngine. Two pages in the same session must report the
//! same values; two sessions should differ.
//!
//! Generation is seed-based so a session can be replayed: pass the same seed
//! to [`Fingerprint::from_seed`] and every value comes back identical. This is
//! what stygian does to keep canvas/audio noise stable across navigations
//! within one session.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const SCREEN_RESOLUTIONS: &[(u32, u32)] = &[
    (1920, 1080),
    (2560, 1440),
    (1440, 900),
    (1366, 768),
    (1536, 864),
    (1280, 800),
    (2560, 1600),
    (1680, 1050),
];

const TIMEZONES: &[&str] = &[
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Toronto",
    "Europe/London",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Amsterdam",
    "Asia/Tokyo",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Australia/Sydney",
];

/// Languages paired with a sensible secondary so `navigator.languages` looks
/// like a real user (primary + base).
const LANGUAGES: &[(&str, &str)] = &[
    ("en-US", "en"),
    ("en-GB", "en"),
    ("en-AU", "en"),
    ("en-CA", "en"),
    ("fr-FR", "fr"),
    ("de-DE", "de"),
    ("es-ES", "es"),
    ("it-IT", "it"),
    ("pt-BR", "pt"),
    ("ja-JP", "ja"),
    ("zh-CN", "zh"),
    ("zh-TW", "zh"),
];

const HARDWARE_CONCURRENCY: &[u32] = &[4, 8, 12, 16];
const DEVICE_MEMORY: &[u32] = &[4, 8, 16];

/// (vendor, renderer, platform, ua). WebGL profile must match the platform
/// or fingerprintjs flags the mismatch.
const WEBGL_PROFILES: &[(&str, &str, &str, &str)] = &[
    (
        "Intel Inc.",
        "Intel Iris OpenGL Engine",
        "MacIntel",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    ),
    (
        "Google Inc. (Intel)",
        "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)",
        "Win32",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    ),
    (
        "Google Inc. (NVIDIA)",
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)",
        "Win32",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    ),
    (
        "Google Inc. (AMD)",
        "ANGLE (AMD, AMD Radeon RX 6700 XT Direct3D11 vs_5_0 ps_5_0, D3D11)",
        "Win32",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    ),
    (
        "Intel Inc.",
        "Intel(R) HD Graphics 530",
        "Linux x86_64",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    ),
    (
        "Google Inc. (Intel)",
        "ANGLE (Intel, Intel(R) Iris(R) Xe Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)",
        "Win32",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    ),
];

pub const WINDOWS_FONTS: &[&str] = &[
    "Arial", "Calibri", "Cambria", "Cambria Math", "Candara", "Comic Sans MS",
    "Consolas", "Constantia", "Corbel", "Courier New", "Ebrima", "Franklin Gothic Medium",
    "Gabriola", "Gadugi", "Georgia", "Javanese Text", "Leelawadee UI", "Lucida Console",
    "Lucida Sans Unicode", "Malgun Gothic", "Microsoft Sans Serif", "MS Gothic",
    "MS PGothic", "MS UI Gothic", "MV Boli", "Palatino Linotype", "Segoe Print",
    "Segoe Script", "Segoe UI", "Segoe UI Emoji", "Segoe UI Historic", "Segoe UI Symbol",
    "SimSun", "Sitka", "Sylfaen", "Tahoma", "Times New Roman", "Trebuchet MS",
    "Verdana", "Yu Gothic", "Yu Gothic UI",
];

pub const MACOS_FONTS: &[&str] = &[
    "American Typewriter", "Andale Mono", "Apple Color Emoji", "Apple SD Gothic Neo",
    "Arial", "Arial Black", "Arial Hebrew", "Arial Narrow", "Arial Rounded MT Bold",
    "Avenir", "Avenir Next", "Avenir Next Condensed", "Baskerville", "Big Caslon",
    "Bodoni 72", "Bradley Hand", "Brush Script MT", "Chalkboard", "Chalkduster",
    "Charter", "Cochin", "Comic Sans MS", "Copperplate", "Courier", "Courier New",
    "DIN Alternate", "DIN Condensed", "Didot", "Futura", "Geneva", "Georgia",
    "Gill Sans", "Helvetica", "Helvetica Neue", "Herculanum", "Hoefler Text", "Impact",
    "Iowan Old Style", "Lucida Grande", "Lucida Sans Unicode", "Luminari", "Menlo",
    "Microsoft Sans Serif", "Monaco", "Noteworthy", "Optima", "Palatino", "Papyrus",
    "Phosphate", "Rockwell", "SF Pro Display", "SF Pro Text", "Savoye LET", "SignPainter",
    "Skia", "Snell Roundhand", "Tahoma", "Times", "Times New Roman", "Trebuchet MS",
    "Verdana", "Zapfino",
];

pub const LINUX_FONTS: &[&str] = &[
    "DejaVu Sans", "DejaVu Sans Mono", "DejaVu Serif", "Liberation Sans",
    "Liberation Mono", "Liberation Serif", "Ubuntu", "Ubuntu Mono", "Ubuntu Condensed",
    "Cantarell", "Droid Sans", "Droid Sans Mono", "Noto Sans", "Noto Sans CJK",
    "Noto Serif", "Noto Mono", "Cousine", "Tinos", "Arial", "Courier New",
    "Georgia", "Times New Roman", "Trebuchet MS", "Verdana",
];

/// A pre-baked fingerprint profile that callers can pass instead of letting
/// the session pick one at random. Useful for replaying a known-good session
/// or pinning a specific identity across multiple pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintProfile {
    pub user_agent: String,
    pub platform: String,
    pub language: String,
    pub secondary_language: String,
    pub timezone: String,
    pub screen: (u32, u32),
    pub hardware_concurrency: u32,
    pub device_memory: u32,
    pub webgl_vendor: String,
    pub webgl_renderer: String,
    pub fonts: Vec<String>,
    pub noise_seed: u64,
}

impl FingerprintProfile {
    pub fn to_fingerprint(&self) -> Fingerprint {
        Fingerprint {
            user_agent: self.user_agent.clone(),
            screen_resolution: self.screen,
            timezone: self.timezone.clone(),
            language: self.language.clone(),
            secondary_language: self.secondary_language.clone(),
            platform: self.platform.clone(),
            hardware_concurrency: self.hardware_concurrency,
            device_memory: self.device_memory,
            webgl_vendor: Some(self.webgl_vendor.clone()),
            webgl_renderer: Some(self.webgl_renderer.clone()),
            canvas_noise: true,
            audio_noise: true,
            fonts: self.fonts.clone(),
            noise_seed: self.noise_seed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fingerprint {
    pub user_agent: String,
    pub screen_resolution: (u32, u32),
    pub timezone: String,
    pub language: String,
    pub secondary_language: String,
    pub platform: String,
    pub hardware_concurrency: u32,
    pub device_memory: u32,
    pub webgl_vendor: Option<String>,
    pub webgl_renderer: Option<String>,
    pub canvas_noise: bool,
    pub audio_noise: bool,
    pub fonts: Vec<String>,
    /// Deterministic seed for canvas/audio/clientRects noise. Same seed → same
    /// noise, so a session that re-navigates keeps its fingerprint stable.
    pub noise_seed: u64,
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self {
            user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
                .to_string(),
            screen_resolution: (1920, 1080),
            timezone: "America/Los_Angeles".to_string(),
            language: "en-US".to_string(),
            secondary_language: "en".to_string(),
            platform: "MacIntel".to_string(),
            hardware_concurrency: 8,
            device_memory: 8,
            webgl_vendor: Some("Intel Inc.".to_string()),
            webgl_renderer: Some("Intel Iris OpenGL Engine".to_string()),
            canvas_noise: true,
            audio_noise: true,
            fonts: Vec::new(),
            noise_seed: 0,
        }
    }
}

/// SplitMix64 — fast, deterministic, good distribution. Used both for
/// fingerprint generation and for the NoiseEngine.
const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn rng(seed: u64, step: u64) -> u64 {
    let mut s = seed.wrapping_add(step.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    splitmix64(&mut s)
}

fn pick<T: Copy>(arr: &[T], seed: u64) -> T {
    arr[(seed % arr.len() as u64) as usize]
}

impl Fingerprint {
    /// Picks a fresh random fingerprint using the wall clock as a seed.
    pub fn random() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0x5a5a_5a5a_5a5a_5a5a, |d| {
                d.as_secs() ^ u64::from(d.subsec_nanos())
            });
        Self::from_seed(seed)
    }

    /// Deterministic generation: same seed → same fingerprint. The seed is
    /// also stored as `noise_seed` so canvas/audio noise stays stable for
    /// the session.
    pub fn from_seed(seed: u64) -> Self {
        let res = pick(SCREEN_RESOLUTIONS, rng(seed, 1));
        let tz = pick(TIMEZONES, rng(seed, 2));
        let (lang, lang2) = pick(LANGUAGES, rng(seed, 3));
        let hw = pick(HARDWARE_CONCURRENCY, rng(seed, 4));
        let dm = pick(DEVICE_MEMORY, rng(seed, 5));
        let (wv, wr, platform, ua) = pick(WEBGL_PROFILES, rng(seed, 6));
        let fonts: Vec<String> = if platform == "Win32" {
            WINDOWS_FONTS.iter().map(|s| s.to_string()).collect()
        } else if platform == "Linux x86_64" {
            LINUX_FONTS.iter().map(|s| s.to_string()).collect()
        } else {
            MACOS_FONTS.iter().map(|s| s.to_string()).collect()
        };
        Self {
            user_agent: ua.to_string(),
            screen_resolution: res,
            timezone: tz.to_string(),
            language: lang.to_string(),
            secondary_language: lang2.to_string(),
            platform: platform.to_string(),
            hardware_concurrency: hw,
            device_memory: dm,
            webgl_vendor: Some(wv.to_string()),
            webgl_renderer: Some(wr.to_string()),
            canvas_noise: true,
            audio_noise: true,
            fonts,
            noise_seed: seed,
        }
    }

    /// FNV-1a 64-bit signature of the identity fields. Used for logging so an
    /// operator can confirm two sessions ended up with different identities
    /// without dumping the full struct.
    pub fn signature(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce3_6422_2325;
        for b in self.user_agent.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        for b in self.timezone.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        for b in self.platform.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        format!("fnv64:{h:016x}")
    }

    /// Assembles the full `addScriptToEvaluateOnNewDocument` payload.
    pub fn injection_script(&self) -> String {
        super::inject::build_injection_script(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_seed_is_deterministic() {
        let a = Fingerprint::from_seed(12345);
        let b = Fingerprint::from_seed(12345);
        assert_eq!(a.user_agent, b.user_agent);
        assert_eq!(a.platform, b.platform);
        assert_eq!(a.timezone, b.timezone);
        assert_eq!(a.noise_seed, b.noise_seed);
    }

    #[test]
    fn different_seeds_usually_differ() {
        let a = Fingerprint::from_seed(1);
        let b = Fingerprint::from_seed(2);
        // Not every field is guaranteed to differ, but the signature should
        // almost always be different.
        assert_ne!(a.signature(), b.signature());
    }

    #[test]
    fn platform_matches_user_agent() {
        for seed in 0..32 {
            let fp = Fingerprint::from_seed(seed);
            if fp.platform == "Win32" {
                assert!(fp.user_agent.contains("Windows"));
            } else if fp.platform == "Linux x86_64" {
                assert!(fp.user_agent.contains("Linux"));
            } else if fp.platform == "MacIntel" {
                assert!(fp.user_agent.contains("Macintosh"));
            }
        }
    }
}
