//! The Cast v2 wire protocol: TLS to the device's port 8009, each frame a
//! 4-byte big-endian length followed by a protobuf `CastMessage`
//! (cast_channel.proto) whose payload is a JSON string.
//!
//! Cast devices present a certificate chained to Google's device CA, not to a
//! public root, so the certificate itself isn't checked (every sender does
//! the same); the handshake signature still is.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{self, CryptoProvider};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{self, ClientConfig, DigitallySignedStruct, SignatureScheme};

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

pub const SENDER: &str = "sender-0";
pub const RECEIVER: &str = "receiver-0";

/// Frames larger than this are not Cast messages.
const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    /// JSON text
    pub payload: String,
}

impl Message {
    pub fn new(destination: &str, namespace: &str, payload: String) -> Self {
        Message {
            source: SENDER.into(),
            destination: destination.into(),
            namespace: namespace.into(),
            payload,
        }
    }
}

// --------------------------------------------------------------- protobuf

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, field: u64, s: &str) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// The protobuf body of a `CastMessage` (protocol CASTV2_1_0, string payload).
pub fn encode(m: &Message) -> Vec<u8> {
    let mut out = Vec::with_capacity(m.payload.len() + 128);
    put_varint(&mut out, 1 << 3); // protocol_version
    put_varint(&mut out, 0);
    put_str(&mut out, 2, &m.source);
    put_str(&mut out, 3, &m.destination);
    put_str(&mut out, 4, &m.namespace);
    put_varint(&mut out, 5 << 3); // payload_type STRING
    put_varint(&mut out, 0);
    put_str(&mut out, 6, &m.payload);
    out
}

fn get_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return Some(v);
        }
    }
    None
}

/// Parses a `CastMessage`; `None` for malformed or binary-payload messages
/// (device authentication, which this sender doesn't use).
pub fn decode(buf: &[u8]) -> Option<Message> {
    let mut m = Message {
        source: String::new(),
        destination: String::new(),
        namespace: String::new(),
        payload: String::new(),
    };
    let mut has_payload = false;
    let mut pos = 0;
    while pos < buf.len() {
        let key = get_varint(buf, &mut pos)?;
        match key & 7 {
            0 => {
                get_varint(buf, &mut pos)?;
            }
            2 => {
                let len = usize::try_from(get_varint(buf, &mut pos)?).ok()?;
                let bytes = buf.get(pos..pos.checked_add(len)?)?;
                pos += len;
                let text = || String::from_utf8(bytes.to_vec()).ok();
                match key >> 3 {
                    2 => m.source = text()?,
                    3 => m.destination = text()?,
                    4 => m.namespace = text()?,
                    6 => {
                        m.payload = text()?;
                        has_payload = true;
                    }
                    _ => {}
                }
            }
            5 => pos += 4,
            1 => pos += 8,
            _ => return None,
        }
    }
    has_payload.then_some(m)
}

// --------------------------------------------------------------- TLS

/// Accepts any server certificate. Deliberate: Chromecasts present a
/// self-signed certificate (device authentication happens over the Cast
/// channel, which this sender skips), and the address comes from mDNS or the
/// user on the local network. Handshake signatures are still verified, so
/// the session is encrypted — just not authenticated. Never reuse this for
/// anything that talks to the internet.
#[derive(Debug)]
struct DeviceCert(Arc<CryptoProvider>);

impl ServerCertVerifier for DeviceCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

type Tls = TlsStream<TcpStream>;

/// For each of the TCP connect and the TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);

pub struct Reader(ReadHalf<Tls>);
pub struct Writer(WriteHalf<Tls>);

/// Opens the Cast channel to a device; also returns this computer's address
/// on the route to it (where the device can reach our relay).
pub async fn connect(addr: SocketAddr) -> io::Result<(Reader, Writer, IpAddr)> {
    let provider = Arc::new(crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(DeviceCert(provider)))
        .with_no_client_auth();
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the device did not answer"))??;
    tcp.set_nodelay(true)?;
    let local = tcp.local_addr()?.ip();
    let name = ServerName::IpAddress(addr.ip().into());
    let tls = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the device did not answer"))??;
    let (r, w) = tokio::io::split(tls);
    Ok((Reader(r), Writer(w), local))
}

impl Reader {
    /// The next string message; binary ones are skipped.
    pub async fn next(&mut self) -> io::Result<Message> {
        loop {
            let len = self.0.read_u32().await? as usize;
            if len > MAX_FRAME {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "oversized cast frame",
                ));
            }
            let mut buf = vec![0; len];
            self.0.read_exact(&mut buf).await?;
            if let Some(m) = decode(&buf) {
                return Ok(m);
            }
        }
    }
}

impl Writer {
    pub async fn send(&mut self, m: &Message) -> io::Result<()> {
        let body = encode(m);
        let mut frame = Vec::with_capacity(body.len() + 4);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        self.0.write_all(&frame).await?;
        self.0.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_messages() {
        let m = Message::new(
            RECEIVER,
            NS_RECEIVER,
            r#"{"type":"LAUNCH","appId":"CC1AD845","requestId":1}"#.into(),
        );
        let bytes = encode(&m);
        // protocol 0, then source "sender-0"
        assert_eq!(&bytes[..4], &[0x08, 0x00, 0x12, 0x08]);
        assert_eq!(decode(&bytes), Some(m));
    }

    #[test]
    fn decodes_long_payloads_and_skips_binary() {
        let m = Message::new("web-5", NS_MEDIA, "x".repeat(300)); // two-byte length varint
        assert_eq!(decode(&encode(&m)).unwrap().payload.len(), 300);
        // payload_type BINARY with payload_binary (field 7), no utf8 payload
        let mut bin = vec![0x08, 0x00];
        put_str(&mut bin, 2, "receiver-0");
        put_str(&mut bin, 3, "sender-0");
        put_str(&mut bin, 4, "urn:x-cast:com.google.cast.tp.deviceauth");
        bin.extend_from_slice(&[0x28, 0x01, 0x3a, 0x02, 0xab, 0xcd]);
        assert_eq!(decode(&bin), None);
        // truncated
        assert_eq!(decode(&encode(&m)[..20]), None);
    }
}
