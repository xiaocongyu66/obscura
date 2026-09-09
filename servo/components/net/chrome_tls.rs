//! Chrome TLS fingerprints, vendored from wreq-util's emulation table
//! (wreq-util 3.0.0-rc.12, `emulate/profile/chrome/tls.rs` + the
//! `mod_generator!` invocations in `chrome.rs`).
//!
//! The Servo HTTP stack applies these directly on the BoringSSL connector
//! (btls) instead of relying on rustls, whose ClientHello is trivially
//! distinguishable from a real Chrome. Ciphers, sigalgs, TLS 1.2 min / 1.3
//! max, OCSP stapling, signed cert timestamps, ALPS on HTTP/2 and the
//! Brotli certificate compressor are shared by every version — only the
//! flags below and the key-share curve list differ.

/// The TLS 1.3 + ECDHE cipher suites Chrome offers, in offer order.
pub const CHROME_CIPHER_LIST: &str = concat!(
    "TLS_AES_128_GCM_SHA256:",
    "TLS_AES_256_GCM_SHA384:",
    "TLS_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:",
    "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:",
    "TLS_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_RSA_WITH_AES_128_CBC_SHA:",
    "TLS_RSA_WITH_AES_256_CBC_SHA"
);

/// The signature algorithms Chrome offers, in order.
pub const CHROME_SIGALGS_LIST: &str = concat!(
    "ecdsa_secp256r1_sha256:",
    "rsa_pss_rsae_sha256:",
    "rsa_pkcs1_sha256:",
    "ecdsa_secp384r1_sha384:",
    "rsa_pss_rsae_sha384:",
    "rsa_pkcs1_sha384:",
    "rsa_pss_rsae_sha512:",
    "rsa_pkcs1_sha512"
);

#[derive(Clone, Copy, Debug)]
pub struct ChromeTlsProfile {
    pub enable_ech_grease: bool,
    pub permute_extensions: bool,
    pub pre_shared_key: bool,
    pub alps_use_new_codepoint: bool,
    pub curves: &'static str,
}

/// Default when the version has no entry: profile of Chrome 131.
pub const DEFAULT_PROFILE: ChromeTlsProfile = ChromeTlsProfile {
    enable_ech_grease: true,
    permute_extensions: true,
    pre_shared_key: true,
    alps_use_new_codepoint: false,
    curves: "X25519MLKEM768:X25519:P-256:P-384",
};

/// Per-version profile resolved from wreq-util's inheritance chain.
pub fn profile_for(chrome_version: u32) -> ChromeTlsProfile {
    match chrome_version {
        100..=101 => ChromeTlsProfile { enable_ech_grease: false, permute_extensions: false, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        104 => ChromeTlsProfile { enable_ech_grease: false, permute_extensions: false, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        105 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: false, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        106..=109 => ChromeTlsProfile { enable_ech_grease: false, permute_extensions: true, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        110 => ChromeTlsProfile { enable_ech_grease: false, permute_extensions: false, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        114 => ChromeTlsProfile { enable_ech_grease: false, permute_extensions: true, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        116 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        117 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        118..=119 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: false, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        120 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        123 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: false, curves: "X25519:P-256:P-384" },
        124 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: false, curves: "X25519Kyber768Draft00:X25519:P-256:P-384" },
        126..=130 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: false, curves: "X25519Kyber768Draft00:X25519:P-256:P-384" },
        131 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: false, curves: "X25519MLKEM768:X25519:P-256:P-384" },
        132..=148 => ChromeTlsProfile { enable_ech_grease: true, permute_extensions: true, pre_shared_key: true, alps_use_new_codepoint: true, curves: "X25519MLKEM768:X25519:P-256:P-384" },
        _ => DEFAULT_PROFILE,
    }
}
