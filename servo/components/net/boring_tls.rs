/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The btls (BoringSSL) TLS adapter.
//!
//! Everything that builds or drives a TLS connection lives here, so the
//! fingerprint policy has exactly one home:
//!
//! - [`ChromeTlsConfig`] — the per-context policy (Chrome version, CA
//!   sources, error handling, certificate overrides).
//! - [`build_ssl_connector`] — applies the Chrome fingerprint from
//!   [`crate::chrome_tls`] onto a btls [`SslConnector`] and wires up the
//!   certificate store and the override/verification callback.
//! - [`connect_tls`] — performs the handshake on an established TCP
//!   stream. Both the HTTP client (see `connector.rs`) and the WebSocket
//!   loader call this; neither knows anything about BoringSSL details.

use std::future;
use std::pin::Pin;
use btls::error::ErrorStack;
use btls::ssl::{
    CertificateCompressionAlgorithm, SslConnector, SslMethod, SslRef, SslVerifyMode, SslVersion,
};
use btls::x509::store::X509StoreBuilder;
use btls::x509::X509;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_btls::SslStream;

use crate::chrome_tls::{self, ChromeTlsProfile};
use crate::connector::CertificateErrorOverrideManager;

/// Registers the Brotli certificate compressor (RFC 8879). Chrome offers
/// exactly this one algorithm in its ClientHello.
#[derive(Debug)]
struct BrotliTlsCompressor;

impl btls::ssl::CertificateCompressor for BrotliTlsCompressor {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
    const CAN_COMPRESS: bool = true;
    const CAN_DECOMPRESS: bool = true;

    fn compress<W: std::io::Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        let mut writer = brotli::CompressorWriter::new(output, input.len(), 11, 32);
        std::io::Write::write_all(&mut writer, input)?;
        std::io::Write::flush(&mut writer)
    }

    fn decompress<W: std::io::Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        let mut reader = brotli::Decompressor::new(input, 4096);
        std::io::copy(&mut reader, output).map(|_| ())
    }
}

/// The TLS policy for this context. Built once per resource thread by
/// [`crate::connector::create_tls_config`].
#[derive(Clone)]
pub struct ChromeTlsConfig {
    pub chrome_version: u32,
    pub ca_override: Vec<Vec<u8>>,
    pub ignore_certificate_errors: bool,
    pub override_manager: CertificateErrorOverrideManager,
}

/// Connection-level fingerprint pieces. BoringSSL keeps ECH grease and
/// ALPS on the SSL object rather than the context, so they are applied
/// per handshake in `connect_tls`.
fn apply_connection_settings(
    ssl: &mut SslRef,
    profile: ChromeTlsProfile,
    alpn_mode: AlpnMode,
) {
    if profile.enable_ech_grease {
        ssl.set_enable_ech_grease(true);
    }
    if alpn_mode == AlpnMode::Browser {
        if profile.alps_use_new_codepoint {
            ssl.set_alps_use_new_codepoint(true);
        }
        if let Err(error) = ssl.add_application_settings(b"h2") {
            log::debug!("ALPS not advertised: {error:?}");
        }
    }
}

/// Build a btls [`SslConnector`] carrying the full Chrome fingerprint.
///
/// This is cheap enough to call per connection (context creation is a few
/// microseconds), which keeps certificate overrides immediately effective
/// without any cache invalidation machinery.
pub fn build_ssl_connector(config: &ChromeTlsConfig) -> Result<SslConnector, ErrorStack> {
    build_ssl_connector_for(config, AlpnMode::Browser)
}

/// ALPN advertisement policy: normal browsing negotiates h2 + HTTP/1.1,
/// while WebSocket connections speak HTTP/1.1 only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlpnMode {
    Browser,
    Http1Only,
}

pub fn build_ssl_connector_for(
    config: &ChromeTlsConfig,
    alpn_mode: AlpnMode,
) -> Result<SslConnector, ErrorStack> {
    let chrome_version = if config.chrome_version != 0 {
        config.chrome_version
    } else {
        ACTIVE_CHROME_VERSION.load(std::sync::atomic::Ordering::Relaxed)
    };
    let profile = chrome_tls::profile_for(chrome_version);
    let mut builder = SslConnector::builder(SslMethod::tls())?;

    // --- ClientHello fingerprint (context level) ---
    builder.set_grease_enabled(true);
    builder.enable_ocsp_stapling();
    builder.enable_signed_cert_timestamps();
    builder.set_curves_list(profile.curves)?;
    builder.set_sigalgs_list(chrome_tls::CHROME_SIGALGS_LIST)?;
    builder.set_cipher_list(chrome_tls::CHROME_CIPHER_LIST)?;
    builder.set_permute_extensions(profile.permute_extensions);
    builder.set_aes_hw_override(true);
    builder.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_3))?;
    match alpn_mode {
        AlpnMode::Browser => {
            // ALPN: h2 preferred, HTTP/1.1 fallback (hyper negotiates from here).
            builder.set_alpn_protos(b"\x02h2\x08http/1.1")?;
        },
        AlpnMode::Http1Only => {
            builder.set_alpn_protos(b"\x08http/1.1")?;
        },
    }
    builder
        .add_certificate_compression_algorithm(BrotliTlsCompressor)
        .ok();

    // --- Verification policy ---
    if config.ignore_certificate_errors {
        builder.set_verify(SslVerifyMode::NONE);
    } else {
        builder.set_verify(SslVerifyMode::PEER);

        let mut store = X509StoreBuilder::new()?;
        // Platform trust store. On Linux this probes the standard
        // directories; embedders with private CAs supply them through
        // `CACertificates::Override`, which land in the same store below.
        store.set_default_paths()?;
        for der in &config.ca_override {
            match X509::from_der(der) {
                Ok(cert) => {
                    if let Err(error) = store.add_cert(cert) {
                        log::warn!("Could not add override CA: {error:?}");
                    }
                },
                Err(error) => log::warn!("Override CA is not valid DER: {error:?}"),
            }
        }
        builder.set_cert_store(store.build());

        // Certificate error overrides: when the chain fails to verify,
        // accept it if the offending certificate was explicitly allowed
        // (the devtools "accept bad certificates" flow). Failures are
        // recorded by DER so they can be surfaced later.
        let manager = config.override_manager.clone();
        builder.set_verify_callback(SslVerifyMode::PEER, move |preverify_ok, ctx| {
            if preverify_ok {
                return true;
            }
            let Some(cert) = ctx.current_cert() else {
                return false;
            };
            let Ok(der) = cert.to_der() else {
                return false;
            };
            manager.on_verification_failure(der)
        });
    }

    Ok(builder.build())
}

/// Drive a btls handshake to completion on an established TCP stream.
///
/// The returned stream is ready for use by hyper or the WebSocket loader.
pub async fn connect_tls<S>(
    connector: &SslConnector,
    host: &str,
    tcp: S,
    chrome_version: u32,
    alpn_mode: AlpnMode,
) -> Result<SslStream<S>, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let chrome_version = if chrome_version != 0 {
        chrome_version
    } else {
        ACTIVE_CHROME_VERSION.load(std::sync::atomic::Ordering::Relaxed)
    };
    let profile = chrome_tls::profile_for(chrome_version);
    let configuration = connector
        .configure()
        .map_err(|e| format!("TLS context: {e:?}"))?;
    let mut ssl = configuration
        .into_ssl(host)
        .map_err(|e| format!("TLS setup for {host}: {e:?}"))?;

    apply_connection_settings(&mut ssl, profile, alpn_mode);

    let mut stream = SslStream::new(ssl, tcp).map_err(|e| format!("TLS stream: {e:?}"))?;
    let mut pinned = Pin::new(&mut stream);
    future::poll_fn(|cx| pinned.as_mut().poll_connect(cx))
        .await
        .map_err(|e| format!("TLS handshake with {host}: {e}"))?;
    Ok(stream)
}

/// The Chrome version applied to every TLS connection this process makes.
/// The embedder sets it alongside the user-agent preference; since each
/// obscura session runs in its own process, a process-global is the
/// natural scope and keeps the UA and the TLS fingerprint coherent.
pub static ACTIVE_CHROME_VERSION: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(131);

/// Set the Chrome version used for all future TLS handshakes.
pub fn set_active_chrome_version(version: u32) {
    ACTIVE_CHROME_VERSION.store(version, std::sync::atomic::Ordering::Relaxed);
}
