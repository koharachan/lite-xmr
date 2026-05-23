use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::{TlsConnector, rustls};
use tracing::{debug, info};

pub enum StratumTransport {
    Tcp(TcpStream),
    Tls(TlsStream<TcpStream>),
}

impl StratumTransport {
    pub async fn connect(addr: &str, use_tls: bool) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        debug!("TCP 连接已建立: {}", addr);

        if use_tls {
            let connector = TlsConnector::from(Arc::new(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
                    .with_no_client_auth(),
            ));

            let server_name: rustls::pki_types::ServerName<'static> = addr
                .split(':')
                .next()
                .unwrap_or("localhost")
                .to_string()
                .try_into()?;

            let tls_stream = connector.connect(server_name, stream).await?;
            info!("TLS 连接已建立: {}", addr);
            Ok(StratumTransport::Tls(tls_stream))
        } else {
            Ok(StratumTransport::Tcp(stream))
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
            StratumTransport::Tls(r) => Pin::new(r).poll_read(cx, buf),
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
            StratumTransport::Tls(r) => Pin::new(r).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_flush(cx),
            StratumTransport::Tls(r) => Pin::new(r).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            StratumTransport::Tcp(r) => Pin::new(r).poll_shutdown(cx),
            StratumTransport::Tls(r) => Pin::new(r).poll_shutdown(cx),
        }
    }
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
