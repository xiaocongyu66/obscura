/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;
use std::time::Duration;
use std::{fmt, io};

use futures::task::{Context, Poll};
use futures::{Future, TryFutureExt};
use http::uri::{Authority, Uri as Destination};
use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use hyper::rt::Executor;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::legacy::connect::{
    Connected, Connection, HttpConnector as HyperHttpConnector,
};
use hyper_util::rt::TokioIo;
use log::warn;
use parking_lot::Mutex;
use servo_config::pref;
use tokio::net::TcpStream;
use tokio_btls::SslStream;
use tower::Service;

use crate::async_runtime::spawn_task;
use crate::boring_tls::{self, ChromeTlsConfig};
use crate::hosts::replace_host;

pub const BUF_SIZE: usize = 32768;

/// ALPN identifier for HTTP/2 (RFC 7540 §3.1).
pub const ALPN_H2: &str = "h2";

#[derive(Clone)]
pub struct ServoHttpConnector {
    inner: HyperHttpConnector,
}

impl ServoHttpConnector {
    fn new() -> ServoHttpConnector {
        let mut inner = HyperHttpConnector::new();
        inner.enforce_http(false);
        inner.set_happy_eyeballs_timeout(None);
        inner.set_connect_timeout(Some(Duration::from_secs(pref!(network_connection_timeout))));
        ServoHttpConnector { inner }
    }
}

impl Service<Destination> for ServoHttpConnector {
    type Response = TokioIo<TcpStream>;
    type Error = ConnectionError;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<TokioIo<TcpStream>, ConnectionError>> + Send>>;

    fn call(&mut self, dest: Destination) -> Self::Future {
        // Perform host replacement when making the actual TCP connection.
        let mut new_dest = dest.clone();
        let mut parts = dest.into_parts();

        if let Some(auth) = parts.authority {
            let host = auth.host();
            let host = replace_host(host);

            let authority = if let Some(port) = auth.port() {
                format!("{}:{}", host, port.as_str())
            } else {
                (*host).to_string()
            };

            if let Ok(authority) = Authority::from_maybe_shared(authority) {
                parts.authority = Some(authority);
                if let Ok(dest) = Destination::from_parts(parts) {
                    new_dest = dest
                }
            }
        }

        Box::pin(
            self.inner
                .call(new_dest)
                .map_err(|e| ConnectionError::HttpError(format!("{e}"))),
        )
    }

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A stream that is either plain TCP or TLS. This is our own enum so the
/// TLS half can be driven by btls (BoringSSL) instead of rustls — see
/// `boring_tls.rs` for the fingerprint policy.
#[derive(Debug)]
pub enum MaybeHttpsStream<S> {
    Plain(S),
    Https(TokioIo<SslStream<S>>),
}

impl<S> MaybeHttpsStream<S>
where
    S: Unpin,
{
    fn handshake_info(ssl: &SslStream<S>) -> Option<TlsHandshakeInfo> {
        let ssl = ssl.ssl();
        let protocol_version = Some(ssl.version().to_string());
        let cipher_suite = ssl.current_cipher().map(|c| c.name().to_string());
        let alpn_protocol = ssl
            .selected_alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned());
        let certificate_chain_der = ssl
            .peer_cert_chain()
            .map(|chain| {
                chain
                    .iter()
                    .filter_map(|cert| cert.to_der().ok())
                    .collect()
            })
            .unwrap_or_default();

        Some(TlsHandshakeInfo {
            protocol_version,
            cipher_suite,
            kea_group_name: None,
            signature_scheme_name: None,
            alpn_protocol,
            certificate_chain_der,
            used_ech: false,
        })
    }
}

impl<S> Connection for MaybeHttpsStream<S>
where
    S: Connection + Unpin,
{
    fn connected(&self) -> Connected {
        match self {
            MaybeHttpsStream::Plain(stream) => stream.connected(),
            MaybeHttpsStream::Https(tls) => {
                let negotiated_h2 =
                    tls.inner().ssl().selected_alpn_protocol() == Some(ALPN_H2.as_bytes());
                let connected = tls.inner().get_ref().connected();
                if negotiated_h2 {
                    connected.negotiated_h2()
                } else {
                    connected
                }
            },
        }
    }
}

impl<S> hyper::rt::Read for MaybeHttpsStream<S>
where
    S: tokio::io::AsyncRead + Unpin,
{

    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.get_mut() {
            MaybeHttpsStream::Plain(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            MaybeHttpsStream::Https(tls) => std::pin::Pin::new(tls).poll_read(cx, buf),
        }
    }
}

impl<S> hyper::rt::Write for MaybeHttpsStream<S>
where
    S: tokio::io::AsyncWrite + Unpin,
{

    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.get_mut() {
            MaybeHttpsStream::Plain(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            MaybeHttpsStream::Https(tls) => std::pin::Pin::new(tls).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.get_mut() {
            MaybeHttpsStream::Plain(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            MaybeHttpsStream::Https(tls) => std::pin::Pin::new(tls).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.get_mut() {
            MaybeHttpsStream::Plain(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            MaybeHttpsStream::Https(tls) => std::pin::Pin::new(tls).poll_shutdown(cx),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            MaybeHttpsStream::Plain(stream) => stream.is_write_vectored(),
            MaybeHttpsStream::Https(tls) => tls.is_write_vectored(),
        }
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        match self.get_mut() {
            MaybeHttpsStream::Plain(stream) => {
                std::pin::Pin::new(stream).poll_write_vectored(cx, bufs)
            },
            MaybeHttpsStream::Https(tls) => std::pin::Pin::new(tls).poll_write_vectored(cx, bufs),
        }
    }
}

/// The connector used for every outgoing HTTP(S) request. It performs host
/// replacement, proxy tunneling, and — for `https` — a Chrome-fingerprinted
/// btls handshake via [`boring_tls::connect_tls`].
#[derive(Clone)]
pub struct ChromeHttpsConnector {
    http: ProxyConnector,
    tls: ChromeTlsConfig,
}

impl ChromeHttpsConnector {
    pub fn new(tls: ChromeTlsConfig) -> Self {
        ChromeHttpsConnector {
            http: ProxyConnector::new(),
            tls,
        }
    }
}

impl Service<Destination> for ChromeHttpsConnector {
    type Response = MaybeHttpsStream<TokioIo<TcpStream>>;
    type Error = ConnectionError;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<Self::Response, ConnectionError>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http.poll_ready(cx)
    }

    fn call(&mut self, dest: Destination) -> Self::Future {
        let scheme = dest.scheme_str().map(|s| s.to_string());
        let host = dest.host().map(|h| h.to_string());
        let chrome_version = self.tls.chrome_version;
        let tls_config = self.tls.clone();
        let http = self.http.clone();

        Box::pin(async move {
            // ProxyConnector already yields TokioIo<TcpStream>; hyper's
            // Read/Write bounds live on TokioIo, so both MaybeHttpsStream
            // variants carry the wrapper rather than the raw socket.
            let tcp = http.call(dest).await?;
            if scheme.as_deref() != Some("https") {
                return Ok(MaybeHttpsStream::Plain(tcp));
            }
            let host = host
                .ok_or_else(|| ConnectionError::TlsError("destination has no host".into()))?;
            let connector = boring_tls::build_ssl_connector(&tls_config)
                .map_err(|e| ConnectionError::TlsError(format!("TLS context: {e:?}")))?;
            let stream = boring_tls::connect_tls(
                &connector,
                &host,
                tcp,
                chrome_version,
                boring_tls::AlpnMode::Browser,
            )
            .await
            .map_err(ConnectionError::TlsError)?;
            Ok(MaybeHttpsStream::Https(TokioIo::new(stream)))
        })
    }
}

/// Wraps a connector to attach [`TlsHandshakeInfo`] to its streams, which
/// the devtools protocol consumes.
#[derive(Clone)]
pub struct InstrumentedConnector<T> {
    inner: T,
}

impl<T> InstrumentedConnector<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

pub struct InstrumentedStream<T> {
    inner: MaybeHttpsStream<T>,
    tls_info: Option<TlsHandshakeInfo>,
}

impl<T: Unpin> Unpin for InstrumentedStream<T> {}

impl<T> fmt::Debug for InstrumentedStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstrumentedStream")
            .field("tls_info", &self.tls_info)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct TlsHandshakeInfo {
    pub protocol_version: Option<String>,
    pub cipher_suite: Option<String>,
    pub kea_group_name: Option<String>,
    pub signature_scheme_name: Option<String>,
    pub alpn_protocol: Option<String>,
    pub certificate_chain_der: Vec<Vec<u8>>,
    pub used_ech: bool,
}

impl<T> InstrumentedStream<T>
where
    T: Connection + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    fn from_maybe_https_stream(stream: MaybeHttpsStream<T>) -> Self {
        match stream {
            MaybeHttpsStream::Plain(inner) => Self {
                inner: MaybeHttpsStream::Plain(inner),
                tls_info: None,
            },
            MaybeHttpsStream::Https(ref tls) => {
                let tls_info = MaybeHttpsStream::handshake_info(tls.inner());
                Self {
                    inner: stream,
                    tls_info,
                }
            },
        }
    }
}

impl<T> Connection for InstrumentedStream<T>
where
    T: Connection + hyper::rt::Read + hyper::rt::Write + Unpin,
{
    fn connected(&self) -> Connected {
        let connected = match &self.inner {
            MaybeHttpsStream::Plain(stream) => stream.connected(),
            MaybeHttpsStream::Https(stream) => {
                let negotiated_h2 =
                    stream.inner().ssl().selected_alpn_protocol() == Some(ALPN_H2.as_bytes());
                let connected = stream.inner().get_ref().connected();
                if negotiated_h2 {
                    connected.negotiated_h2()
                } else {
                    connected
                }
            },
        };
        if let Some(info) = &self.tls_info {
            connected.extra(info.clone())
        } else {
            connected
        }
    }
}

impl<T> hyper::rt::Read for InstrumentedStream<T>
where
    T: tokio::io::AsyncRead + Unpin,
    MaybeHttpsStream<T>: hyper::rt::Read,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T> hyper::rt::Write for InstrumentedStream<T>
where
    T: tokio::io::AsyncWrite + Unpin,
    MaybeHttpsStream<T>: hyper::rt::Write,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

impl Service<Destination> for InstrumentedConnector<ChromeHttpsConnector> {
    type Response = InstrumentedStream<TokioIo<TcpStream>>;
    type Error = BoxError;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<InstrumentedStream<TokioIo<TcpStream>>, BoxError>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready(cx)
            .map_err(|e| -> BoxError { e.into() })
    }

    fn call(&mut self, dst: Destination) -> Self::Future {
        let future = self.inner.call(dst);
        Box::pin(async move {
            let stream = future.await.map_err(|error| -> BoxError { error.into() })?;
            Ok(InstrumentedStream::from_maybe_https_stream(stream))
        })
    }
}

pub type BoxedBody = BoxBody<Bytes, hyper::Error>;

#[derive(Debug)]
/// The error type for the MaybeProxyConnector
pub enum ConnectionError {
    HttpError(String),
    // It looks like currently the type is not exported.
    ProxyError(String),
    TlsError(String),
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ConnectionError {}

#[derive(Clone)]
/// A proxy connector. This will automatically open a proxy connection if the uri matches the proxy uri.
/// Also respects 'no_proxy'.
pub struct ProxyConnector {
    /// A client without proxy for `no_proxy` matches.
    client: ServoHttpConnector,
    /// Matcher to see if we should forward to the proxy or not.
    matcher: std::sync::Arc<hyper_util::client::proxy::matcher::Matcher>,
}

impl ProxyConnector {
    fn new() -> Self {
        let matcher_builder = hyper_util::client::proxy::matcher::Matcher::builder()
            .http(servo_config::pref!(network_http_proxy_uri))
            .https(servo_config::pref!(network_https_proxy_uri))
            .no(servo_config::pref!(network_http_no_proxy));
        ProxyConnector {
            client: ServoHttpConnector::new(),
            matcher: std::sync::Arc::new(matcher_builder.build()),
        }
    }
}

// Just forward everything to the inner type except that we modify the errors returned.
impl Service<Destination> for ProxyConnector {
    type Response = TokioIo<TcpStream>;
    type Error = ConnectionError;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<TokioIo<TcpStream>, ConnectionError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.client
            .poll_ready(cx)
            .map_err(|e| ConnectionError::ProxyError(format!("{e}")))
    }

    fn call(&mut self, req: Destination) -> Self::Future {
        match self.matcher.intercept(&req) {
            Some(intercept) => {
                let mut tunnel = Tunnel::new(intercept.uri().clone(), self.client.clone());
                let final_tunnel = if let Some(auth) = intercept.basic_auth() {
                    tunnel.with_auth(auth.clone())
                } else {
                    tunnel
                }
                .call(req)
                .map_err(|e| ConnectionError::ProxyError(format!("{e}")));
                Box::pin(final_tunnel)
            },
            None => Box::pin(
                self.client
                    .call(req)
                    .map_err(|e| ConnectionError::ProxyError(format!("{e}"))),
            ),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct CertificateErrorOverrideManagerInternal {
    /// A list of certificates that should be accepted despite encountering
    /// verification errors (DER bytes, as delivered by the override flow).
    overrides: Vec<Vec<u8>>,
    /// Certificates that recently failed verification (DER bytes). The
    /// BoringSSL preverify callback has no host parameter, so unlike the
    /// old rustls verifier these are not keyed by host.
    certificates_failing_to_verify: Vec<Vec<u8>>,
}

/// This data structure is used to track certificate verification errors and overrides.
/// It tracks:
///  - A list of [Certificate]s with verification errors mapped by their [ServerName]
///  - A list of [Certificate]s for which to ignore verification errors.
#[derive(Clone, Debug, Default)]
pub struct CertificateErrorOverrideManager(Arc<Mutex<CertificateErrorOverrideManagerInternal>>);

impl CertificateErrorOverrideManager {
    pub fn new() -> Self {
        Self(Default::default())
    }

    /// Add a certificate to this manager's list of certificates for which to ignore
    /// validation errors.
    pub fn add_override(&self, certificate: &[u8]) {
        self.0.lock().overrides.push(certificate.to_vec());
    }

    /// Given the a string representation of a sever host name, remove information about
    /// a [Certificate] with verification errors. If a certificate with
    /// verification errors was found, return it, otherwise None.
    ///
    /// Note: with the BoringSSL callback the failures are not keyed by
    /// host, so this returns the most recently failed certificate.
    pub(crate) fn remove_certificate_failing_verification(
        &self,
        _host: &str,
    ) -> Option<Vec<u8>> {
        self.0.lock().certificates_failing_to_verify.pop()
    }

    /// The preverify-callback half: called on a failed chain verification.
    /// Returns whether the certificate is explicitly allowed.
    pub(crate) fn on_verification_failure(&self, certificate_der: Vec<u8>) -> bool {
        let mut state = self.0.lock();
        if state.overrides.contains(&certificate_der) {
            return true;
        }
        state
            .certificates_failing_to_verify
            .push(certificate_der);
        false
    }
}

#[derive(Clone, Debug, Default)]
pub enum CACertificates<'de> {
    #[default]
    Default,
    Override(Vec<Vec<u8>>),
}

/// Create a [TlsConfig] describing the TLS policy for this context. The
/// Chrome fingerprint itself is applied per connection in
/// `boring_tls::build_ssl_connector` so certificate overrides take effect
/// immediately.
///
/// The `ignore_certificate_errors` argument ignores all certificate errors.
/// This is used when running the WPT tests.
#[servo_tracing::instrument(skip_all)]
pub fn create_tls_config(
    ca_certificates: CACertificates<'static>,
    ignore_certificate_errors: bool,
    override_manager: CertificateErrorOverrideManager,
) -> TlsConfig {
    // The Chrome version follows the session: the embedder sets it through
    // `boring_tls::set_active_chrome_version` together with the UA
    // preference. Zero here means "read the process-global at connect time".
    TlsConfig {
        chrome_version: 0,
        ca_override: match ca_certificates {
            CACertificates::Default => Vec::new(),
            CACertificates::Override(certificates) => certificates,
        },
        ignore_certificate_errors,
        override_manager,
    }
}

#[derive(Clone)]
struct TokioExecutor {}

impl<F> Executor<F> for TokioExecutor
where
    F: Future<Output = ()> + 'static + std::marker::Send,
{
    fn execute(&self, fut: F) {
        spawn_task(fut);
    }
}

/// Prewarm the TLS stack to speed up the first connection.
///
/// Building the first BoringSSL context and seeding its RNG happen lazily;
/// doing both off the request path keeps the first navigation snappy.
#[inline]
pub fn prewarm_tls() {
    #[servo_tracing::instrument]
    fn prewarm_tls_impl() {
        let mut sink = [0u8; 32];
        // Force the BoringSSL RNG to gather entropy.
        let _ = btls::rand::rand_bytes(&mut sink);
        // Build a throwaway connector so context-level initialization
        // happens now rather than on the first connection.
        let _ = boring_tls::build_ssl_connector(&create_tls_config(
            CACertificates::Default,
            false,
            CertificateErrorOverrideManager::new(),
        ));
    }

    if let Err(error) = std::thread::Builder::new()
        .name("Net-TLS-prewarm".into())
        .spawn(prewarm_tls_impl)
    {
        warn!("Failed to spawn thread to prewarm TLS: {error:?}");
    }
}


pub type TlsConfig = ChromeTlsConfig;

pub type ServoClient = Client<InstrumentedConnector<ChromeHttpsConnector>, BoxedBody>;

pub fn create_http_client(tls_config: TlsConfig) -> ServoClient {
    let connector = ChromeHttpsConnector::new(tls_config);

    Client::builder(TokioExecutor {}).build(InstrumentedConnector::new(connector))
}
