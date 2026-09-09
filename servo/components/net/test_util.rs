/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use core::convert::Infallible;
use std::fs::File;
use std::io::{self, BufReader};
use std::net::TcpListener as StdTcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use crossbeam_channel::unbounded;
use embedder_traits::{EmbedderMsg, EmbedderProxy, EventLoopWaker, GenericEmbedderProxy};
use futures::future::ready;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse};
use hyper_util::rt::tokio::TokioIo;
use net_traits::AsyncRuntime;
use net_traits::blob_url_store::UrlWithBlobClaim;
use servo_default_resources as _;
use servo_url::ServoUrl;
use tokio::net::{TcpListener, TcpStream};
use tokio_btls::SslStream;
use btls::ssl::{SslAcceptor, SslMethod};

use crate::async_runtime::{
    async_runtime_initialized, init_async_runtime, spawn_blocking_task, spawn_task,
};
pub use crate::hosts::replace_host_table;

static ASYNC_RUNTIME: LazyLock<Arc<Mutex<Box<dyn AsyncRuntime>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(init_async_runtime())));

pub fn create_embedder_proxy() -> EmbedderProxy {
    create_generic_embedder_proxy::<EmbedderMsg>()
}

pub fn create_generic_embedder_proxy<T>() -> GenericEmbedderProxy<T> {
    if !async_runtime_initialized() {
        let _init = ASYNC_RUNTIME.clone();
    }
    let (sender, _) = unbounded();
    let event_loop_waker = || {
        struct DummyEventLoopWaker {}
        impl DummyEventLoopWaker {
            fn new() -> DummyEventLoopWaker {
                DummyEventLoopWaker {}
            }
        }
        impl EventLoopWaker for DummyEventLoopWaker {
            fn wake(&self) {}
            fn clone_box(&self) -> Box<dyn EventLoopWaker> {
                Box::new(DummyEventLoopWaker {})
            }
        }

        Box::new(DummyEventLoopWaker::new())
    };

    GenericEmbedderProxy {
        sender: sender,
        event_loop_waker: event_loop_waker(),
    }
}

#[derive(Debug)]
pub struct Server {
    pub close_channel: tokio::sync::oneshot::Sender<()>,
    pub certificates: Option<Vec<CertificateDer<'static>>>,
}

impl Server {
    pub fn close(self) {
        self.close_channel.send(()).expect("err closing server:");
    }
}

pub fn make_server<H>(handler: H) -> (Server, UrlWithBlobClaim)
where
    H: Fn(HyperRequest<Incoming>, &mut HyperResponse<BoxBody<Bytes, hyper::Error>>)
        + Send
        + Sync
        + 'static,
{
    if !async_runtime_initialized() {
        let _ = &*ASYNC_RUNTIME;
    }
    let handler = Arc::new(handler);

    let listener = StdTcpListener::bind("0.0.0.0:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener =
        spawn_blocking_task::<_, TcpListener>(
            async move { TcpListener::from_std(listener).unwrap() },
        );

    let url_string = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let url = UrlWithBlobClaim::new(ServoUrl::parse(&url_string).unwrap(), None);

    let graceful = hyper_util::server::graceful::GracefulShutdown::new();

    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let server = async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream.unwrap().0,
                _val = &mut rx => {
                    let _ = graceful.shutdown();
                    break;
                }
            };

            let handler = handler.clone();

            let stream = stream.into_std().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::new(5, 0)))
                .unwrap();
            let stream = TcpStream::from_std(stream).unwrap();
            let http = http1::Builder::new();
            let conn = http.serve_connection(
                TokioIo::new(stream),
                service_fn(move |req: HyperRequest<Incoming>| {
                    let mut response =
                        HyperResponse::new(Empty::new().map_err(|_| unreachable!()).boxed());
                    handler(req, &mut response);
                    ready(Ok::<_, Infallible>(response))
                }),
            );
            let conn = graceful.watch(conn);
            spawn_task(async move {
                let _ = conn.await;
            });
        }
    };

    let _ = spawn_task(server);
    (
        Server {
            close_channel: tx,
            certificates: None,
        },
        url,
    )
}

/// Given a path to a file containing PEM certificates, load and parse them into
/// a vector of RusTLS [Certificate]s.
fn load_certificates_from_pem(
    path: &PathBuf,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let pem = std::fs::read(path)?;
    Ok(btls::x509::X509::stack_from_pem(&pem)?
        .iter()
        .map(|cert| cert.to_der().map_err(|e| -> Box<dyn std::error::Error> { e.into() }))
        .collect::<Result<Vec<_>, _>>()?)
}

/// Given a path to a file containing PEM keys, load and parse the PKCS8
/// private key into DER bytes.
fn load_private_key_from_file(path: &PathBuf) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let pem = std::fs::read(path)?;
    let key = btls::pkey::PKey::private_key_from_pem(&pem)?;
    Ok(key.private_key_to_der_pkcs8()?)
}

pub fn make_ssl_server<H>(handler: H) -> (Server, UrlWithBlobClaim)
where
    H: Fn(HyperRequest<Incoming>, &mut HyperResponse<BoxBody<Bytes, hyper::Error>>)
        + Send
        + Sync
        + 'static,
{
    if !async_runtime_initialized() {
        let _ = &*ASYNC_RUNTIME;
    }
    let handler = Arc::new(handler);
    let listener = StdTcpListener::bind("[::0]:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener =
        spawn_blocking_task::<_, TcpListener>(
            async move { TcpListener::from_std(listener).unwrap() },
        );

    let url_string = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let url = UrlWithBlobClaim::new(ServoUrl::parse(&url_string).unwrap(), None);

    let cert_path = Path::new("../../resources/self_signed_certificate_for_testing.crt")
        .canonicalize()
        .unwrap();
    let key_path = Path::new("../../resources/privatekey_for_testing.key")
        .canonicalize()
        .unwrap();
    let certificates = load_certificates_from_pem(&cert_path).expect("Invalid certificate");
    let key = load_private_key_from_file(&key_path).expect("Invalid key");

    let mut acceptor_builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls())
        .expect("Could not create SSL acceptor builder");
    let cert = btls::x509::X509::from_der(&certificates[0]).expect("Invalid test certificate");
    let pkey = btls::pkey::PKey::private_key_from_pkcs8(&key).expect("Invalid test key");
    acceptor_builder
        .set_certificate(&cert)
        .and_then(|_| acceptor_builder.set_private_key(&pkey))
        .expect("Could not install test certificate");
    let acceptor = Arc::new(acceptor_builder.build());

    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let server = async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream.unwrap().0,
                _ = &mut rx => break
            };

            let stream = stream.into_std().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::new(5, 0)))
                .unwrap();
            let stream = TcpStream::from_std(stream).unwrap();

            let handler = handler.clone();
            let acceptor = acceptor.clone();

            let acceptor = acceptor.clone();
            let tls = async move {
                let ssl = btls::ssl::Ssl::new(acceptor.context())
                    .map_err(|e| std::io::Error::other(format!("TLS setup: {e:?}")))?;
                let mut tls = SslStream::new(ssl, stream)
                    .map_err(|e| std::io::Error::other(format!("TLS stream: {e:?}")))?;
                std::pin::Pin::new(&mut tls)
                    .accept()
                    .await
                    .map_err(std::io::Error::other)?;
                Ok::<SslStream<tokio::net::TcpStream>, std::io::Error>(tls)
            }
            .await;
            let stream = match tls {
                Ok(stream) => stream,
                Err(_) => {
                    eprintln!("Error handling TLS stream.");
                    continue;
                },
            };

            let _ = http1::Builder::new()
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(move |req: HyperRequest<Incoming>| {
                        let mut response =
                            HyperResponse::new(Empty::new().map_err(|_| unreachable!()).boxed());
                        handler(req, &mut response);
                        ready(Ok::<_, Infallible>(response))
                    }),
                )
                .await;
        }
    };

    spawn_task(server);

    (
        Server {
            close_channel: tx,
            certificates: Some(certificates),
        },
        url,
    )
}

pub fn make_body(bytes: Vec<u8>) -> BoxBody<Bytes, hyper::Error> {
    Full::new(Bytes::from(bytes))
        .map_err(|_| unreachable!())
        .boxed()
}
