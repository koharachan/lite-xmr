use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_openssl::SslStream as OpenSslTlsStream;
use tokio_rustls::client::TlsStream as RustlsTlsStream;
use tokio_rustls::{TlsConnector, rustls};
use tracing::{debug, info, warn};

pub enum StratumTransport {
    Tcp(TcpStream),
    TlsRustls(RustlsTlsStream<TcpStream>),
    TlsOpenSsl(OpenSslTlsStream<TcpStream>),
}

impl StratumTransport {
    pub async fn connect(
        addr: &str,
        use_tls: bool,
        sni_override: Option<&str>,
        tls_allow_12: bool,
        tls_fingerprint: Option<&str>,
        socks5: Option<&str>,
    ) -> anyhow::Result<Self> {
        let endpoint = parse_pool_endpoint(addr, sni_override)?;
        let stream = connect_tcp(&endpoint, socks5).await?;
        debug!("TCP connected {}", endpoint.connect_addr);

        if !use_tls {
            return Ok(StratumTransport::Tcp(stream));
        }

        match connect_rustls(
            stream,
            &endpoint.tls_server_name,
            tls_allow_12,
            tls_fingerprint,
        )
        .await
        {
            Ok(tls_stream) => {
                info!(
                    "TLS connected via rustls {} sni={} overridden={}",
                    endpoint.connect_addr, endpoint.tls_server_name, endpoint.sni_overridden
                );
                Ok(StratumTransport::TlsRustls(tls_stream))
            }
            Err(rustls_err) => {
                warn!(
                    "rustls TLS handshake failed for {}: {}, retrying with OpenSSL",
                    endpoint.connect_addr, rustls_err
                );

                let stream = connect_tcp(&endpoint, socks5).await?;
                let tls_stream = connect_openssl(
                    stream,
                    &endpoint.tls_server_name,
                    endpoint.sni_overridden,
                    tls_allow_12,
                    tls_fingerprint,
                )
                .await?;
                info!(
                    "TLS connected via OpenSSL {} sni={}",
                    endpoint.connect_addr,
                    if endpoint.sni_overridden {
                        endpoint.tls_server_name.as_str()
                    } else {
                        "<disabled>"
                    }
                );
                Ok(StratumTransport::TlsOpenSsl(tls_stream))
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
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_flush(cx),
            StratumTransport::TlsRustls(r) => Pin::new(r).poll_flush(cx),
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_shutdown(cx),
            StratumTransport::TlsRustls(r) => Pin::new(r).poll_shutdown(cx),
            StratumTransport::TlsOpenSsl(r) => Pin::new(r).poll_shutdown(cx),
        }
    }
}

async fn connect_rustls(
    stream: TcpStream,
    server_name: &str,
    tls_allow_12: bool,
    tls_fingerprint: Option<&str>,
) -> anyhow::Result<RustlsTlsStream<TcpStream>> {
    let versions: &[&'static rustls::SupportedProtocolVersion] = if tls_allow_12 {
        &[&rustls::version::TLS13, &rustls::version::TLS12]
    } else {
        &[&rustls::version::TLS13]
    };
    let verifier = PoolCertVerifier::new(tls_fingerprint)?;
    let connector = TlsConnector::from(Arc::new(
        rustls::ClientConfig::builder_with_protocol_versions(versions)
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
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

async fn connect_openssl(
    stream: TcpStream,
    server_name: &str,
    send_sni: bool,
    tls_allow_12: bool,
    tls_fingerprint: Option<&str>,
) -> anyhow::Result<OpenSslTlsStream<TcpStream>> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    builder.set_verify(SslVerifyMode::NONE);
    builder.set_min_proto_version(Some(if tls_allow_12 {
        SslVersion::TLS1_2
    } else {
        SslVersion::TLS1_3
    }))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_3))?;

    let mut config = builder.build().configure()?;
    config.set_use_server_name_indication(send_sni);
    config.set_verify_hostname(false);

    let ssl = config.into_ssl(server_name)?;
    let mut tls_stream = OpenSslTlsStream::new(ssl, stream)?;
    Pin::new(&mut tls_stream).connect().await?;
    if let Some(expected) = Fingerprint::parse_optional(tls_fingerprint)? {
        let cert = tls_stream
            .ssl()
            .peer_certificate()
            .ok_or_else(|| anyhow::anyhow!("TLS pool did not send a certificate"))?;
        let actual = sha256_hex(&cert.to_der()?);
        if actual != expected.hex {
            anyhow::bail!(
                "TLS certificate fingerprint mismatch: expected {}, got {}",
                expected.hex,
                actual
            );
        }
    }
    Ok(tls_stream)
}

struct PoolEndpoint {
    connect_addr: String,
    host: String,
    port: u16,
    tls_server_name: String,
    sni_overridden: bool,
}

fn parse_pool_endpoint(addr: &str, sni_override: Option<&str>) -> anyhow::Result<PoolEndpoint> {
    let raw = addr.trim();
    if raw.is_empty() {
        anyhow::bail!("empty pool address");
    }

    let rest = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    let authority = match rest.find(|c| matches!(c, '/' | '?' | '#')) {
        Some(idx) => &rest[..idx],
        None => rest,
    };
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

    let sni_override = sni_override.map(str::trim).filter(|s| !s.is_empty());
    let tls_server_name = sni_override.unwrap_or(&host).to_string();

    Ok(PoolEndpoint {
        connect_addr,
        host,
        port,
        tls_server_name,
        sni_overridden: sni_override.is_some(),
    })
}

fn parse_port(port: &str, raw: &str) -> anyhow::Result<u16> {
    port.parse::<u16>()
        .map_err(|_| anyhow::anyhow!("invalid port in pool address '{}'", raw))
}

#[derive(Debug)]
struct PoolCertVerifier {
    fingerprint: Option<Fingerprint>,
}

impl PoolCertVerifier {
    fn new(fingerprint: Option<&str>) -> anyhow::Result<Self> {
        Ok(Self {
            fingerprint: Fingerprint::parse_optional(fingerprint)?,
        })
    }
}

impl rustls::client::danger::ServerCertVerifier for PoolCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.fingerprint {
            let actual = sha256_hex(end_entity.as_ref());
            if actual != expected.hex {
                return Err(rustls::Error::General(format!(
                    "TLS certificate fingerprint mismatch: expected {}, got {}",
                    expected.hex, actual
                )));
            }
        }
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

#[derive(Debug, Clone)]
struct Fingerprint {
    hex: String,
}

impl Fingerprint {
    fn parse_optional(raw: Option<&str>) -> anyhow::Result<Option<Self>> {
        let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let hex = raw
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .flat_map(|c| c.to_lowercase())
            .collect::<String>();
        if hex.len() != 64 {
            anyhow::bail!("TLS fingerprint must be a SHA-256 hex digest");
        }
        Ok(Some(Self { hex }))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn connect_tcp(endpoint: &PoolEndpoint, socks5: Option<&str>) -> anyhow::Result<TcpStream> {
    let Some(proxy) = socks5.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(TcpStream::connect(&endpoint.connect_addr).await?);
    };

    let mut stream = TcpStream::connect(proxy).await?;
    socks5_connect(&mut stream, &endpoint.host, endpoint.port).await?;
    debug!(
        "SOCKS5 connected via {} to {}",
        proxy, endpoint.connect_addr
    );
    Ok(stream)
}

async fn socks5_connect(stream: &mut TcpStream, host: &str, port: u16) -> anyhow::Result<()> {
    stream.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut hello = [0u8; 2];
    stream.read_exact(&mut hello).await?;
    if hello != [0x05, 0x00] {
        anyhow::bail!("SOCKS5 proxy requires unsupported authentication");
    }

    let mut req = Vec::with_capacity(7 + host.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00]);
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(ip) => {
                req.push(0x01);
                req.extend_from_slice(&ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                req.push(0x04);
                req.extend_from_slice(&ip.octets());
            }
        }
    } else {
        let host_bytes = host.as_bytes();
        if host_bytes.len() > u8::MAX as usize {
            anyhow::bail!("SOCKS5 host name is too long");
        }
        req.push(0x03);
        req.push(host_bytes.len() as u8);
        req.extend_from_slice(host_bytes);
    }
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).await?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 || head[1] != 0x00 {
        anyhow::bail!("SOCKS5 connect failed with status 0x{:02x}", head[1]);
    }
    match head[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            stream.read_exact(&mut skip).await?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut skip = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut skip).await?;
        }
        0x04 => {
            let mut skip = [0u8; 18];
            stream.read_exact(&mut skip).await?;
        }
        other => anyhow::bail!("SOCKS5 proxy returned invalid address type 0x{:02x}", other),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_uses_custom_sni_without_changing_connect_addr() {
        let endpoint =
            parse_pool_endpoint("wss://203.0.113.10:443/stratum", Some("proxy.example.com"))
                .unwrap();

        assert_eq!(endpoint.connect_addr, "203.0.113.10:443");
        assert_eq!(endpoint.tls_server_name, "proxy.example.com");
        assert!(endpoint.sni_overridden);
    }

    #[test]
    fn endpoint_strips_url_path_and_defaults_sni_to_host() {
        let endpoint = parse_pool_endpoint("stratum+tls://pool.example:443/path", None).unwrap();

        assert_eq!(endpoint.connect_addr, "pool.example:443");
        assert_eq!(endpoint.host, "pool.example");
        assert_eq!(endpoint.port, 443);
        assert_eq!(endpoint.tls_server_name, "pool.example");
        assert!(!endpoint.sni_overridden);
    }

    #[test]
    fn fingerprint_parser_accepts_colon_separated_sha256() {
        let parsed = Fingerprint::parse_optional(Some(
            "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99",
        ))
        .unwrap()
        .unwrap();

        assert_eq!(
            parsed.hex,
            "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
        );
    }
}
