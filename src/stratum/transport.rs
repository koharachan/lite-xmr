use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[cfg(windows)]
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
#[cfg(windows)]
use tokio_openssl::SslStream as OpenSslTlsStream;
use tokio_rustls::client::TlsStream as RustlsTlsStream;
use tokio_rustls::{TlsConnector, rustls};
use tracing::{debug, info, warn};

pub enum StratumTransport {
    Tcp(TcpStream),
    TlsRustls(RustlsTlsStream<TcpStream>),
    #[cfg(windows)]
    TlsOpenSsl(OpenSslTlsStream<TcpStream>),
}

impl StratumTransport {
    pub async fn connect(addr: &str, use_tls: bool) -> anyhow::Result<Self> {
        let endpoint = parse_pool_endpoint(addr)?;
        let stream = TcpStream::connect(&endpoint.connect_addr).await?;
        debug!("TCP connected {}", endpoint.connect_addr);

        if !use_tls {
            return Ok(StratumTransport::Tcp(stream));
        }

        match connect_rustls(stream, &endpoint.tls_server_name).await {
            Ok(tls_stream) => {
                info!("TLS connected via rustls {}", endpoint.connect_addr);
                Ok(StratumTransport::TlsRustls(tls_stream))
            }
            Err(rustls_err) => {
                #[cfg(windows)]
                {
                    warn!(
                        "rustls TLS handshake failed for {}: {}, retrying with OpenSSL",
                        endpoint.connect_addr, rustls_err
                    );

                    let stream = TcpStream::connect(&endpoint.connect_addr).await?;
                    let tls_stream = connect_openssl(stream, &endpoint.tls_server_name).await?;
                    info!("TLS connected via OpenSSL {}", endpoint.connect_addr);
                    Ok(StratumTransport::TlsOpenSsl(tls_stream))
                }

                #[cfg(not(windows))]
                {
                    Err(rustls_err)
                }
            }
        }
    }
}

impl AsyncRead for StratumTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_read(cx, buf),
            StratumTransport::TlsRustls(r) => Pin::new(r).poll_read(cx, buf),
            #[cfg(windows)]
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for StratumTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_write(cx, buf),
            StratumTransport::TlsRustls(r) => Pin::new(r).poll_write(cx, buf),
            #[cfg(windows)]
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_flush(cx),
            StratumTransport::TlsRustls(r) => Pin::new(r).poll_flush(cx),
            #[cfg(windows)]
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_shutdown(cx),
            StratumTransport::TlsRustls(r) => Pin::new(r).poll_shutdown(cx),
            #[cfg(windows)]
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_shutdown(cx),
        }
    }
}

async fn connect_rustls(
    stream: TcpStream,
    server_name: &str,
) -> anyhow::Result<RustlsTlsStream<TcpStream>> {
    let connector = TlsConnector::from(Arc::new(
        rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
            .with_no_client_auth(),
    ));

    let server_name: rustls::pki_types::ServerName<'static> =
        if let Ok(ip) = server_name.parse::<std::net::IpAddr>() {
            rustls::pki_types::ServerName::IpAddress(ip.into())
        } else {
            server_name.to_string().try_into()?
        };
    let tls_stream = connector.connect(server_name, stream).await?;
    Ok(tls_stream)
}

#[cfg(windows)]
async fn connect_openssl(
    stream: TcpStream,
    server_name: &str,
) -> anyhow::Result<OpenSslTlsStream<TcpStream>> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    builder.set_verify(SslVerifyMode::NONE);
    builder.set_min_proto_version(Some(SslVersion::TLS1_3))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_3))?;

    let mut config = builder.build().configure()?;
    config.set_use_server_name_indication(false);
    config.set_verify_hostname(false);

    let ssl = config.into_ssl(server_name)?;
    let mut tls_stream = OpenSslTlsStream::new(ssl, stream)?;
    Pin::new(&mut tls_stream).connect().await?;
    Ok(tls_stream)
}

struct PoolEndpoint {
    connect_addr: String,
    tls_server_name: String,
}

fn parse_pool_endpoint(addr: &str) -> anyhow::Result<PoolEndpoint> {
    let raw = addr.trim();
    if raw.is_empty() {
        anyhow::bail!("empty pool address");
    }

    let authority = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    if authority.is_empty() {
        anyhow::bail!("invalid pool address '{}'", raw);
    }

    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped
            .find(']')
            .ok_or_else(|| anyhow::anyhow!("invalid IPv6 pool address '{}'", raw))?;
        let host = &stripped[..end];
        let rest = &stripped[end + 1..];
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| anyhow::anyhow!("missing port in pool address '{}'", raw))?;
        (host.to_string(), parse_port(port, raw)?)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("missing port in pool address '{}'", raw))?;
        if host.contains(':') {
            anyhow::bail!("IPv6 pool address must use [addr]:port form: '{}'", raw);
        }
        (host.to_string(), parse_port(port, raw)?)
    };

    if host.is_empty() {
        anyhow::bail!("missing host in pool address '{}'", raw);
    }

    let connect_addr = if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };

    Ok(PoolEndpoint {
        connect_addr,
        tls_server_name: host,
    })
}

fn parse_port(port: &str, raw: &str) -> anyhow::Result<u16> {
    port.parse::<u16>()
        .map_err(|_| anyhow::anyhow!("invalid port in pool address '{}'", raw))
}

#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::CryptoProvider::get_default()
            .map(|p| p.signature_verification_algorithms.supported_schemes())
            .unwrap_or_default()
    }
}
