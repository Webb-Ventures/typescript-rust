//! Byte streams the server talks over: plain TCP, or TLS (`server.https`, `https://` and
//! `wss://` proxy targets) with the `tls` feature.
//!
//! A [`Duplex`] is split into an owned reader and writer so one thread can block reading
//! while others write (HMR broadcasts, WebSocket tunnels). For TLS both halves share the
//! `rustls` connection behind a mutex; the reader does its blocking socket read outside the
//! lock and only feeds the bytes in under it.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// A connection split into halves, plus the socket (timeouts, shutdown).
pub struct Duplex {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub tcp: TcpStream,
}

impl std::fmt::Debug for Duplex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Duplex")
            .field("tcp", &self.tcp)
            .finish_non_exhaustive()
    }
}

impl Duplex {
    pub fn plain(tcp: TcpStream) -> io::Result<Duplex> {
        Ok(Duplex {
            reader: Box::new(tcp.try_clone()?),
            writer: Box::new(tcp.try_clone()?),
            tcp,
        })
    }

    pub fn shutdown(&self) {
        let _ = self.tcp.shutdown(Shutdown::Both);
    }
}

/// Opens a connection to `host:port`, over TLS when `tls` (verifying the certificate
/// against the Mozilla roots unless `insecure`).
pub fn connect(host: &str, port: u16, tls: bool, insecure: bool) -> io::Result<Duplex> {
    let host_for_addr = host.trim_start_matches('[').trim_end_matches(']');
    let mut last = None;
    let mut tcp = None;
    for addr in (host_for_addr, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let tcp = match tcp {
        Some(t) => t,
        None => {
            return Err(last.unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no address for {host}"))
            }));
        }
    };
    tcp.set_nodelay(true)?;
    if !tls {
        return Duplex::plain(tcp);
    }
    #[cfg(feature = "tls")]
    {
        tls::connect(tcp, host_for_addr, insecure)
    }
    #[cfg(not(feature = "tls"))]
    {
        let _ = insecure;
        Err(io::Error::other(
            "TLS targets need ts_r_hmr's `tls` feature",
        ))
    }
}

#[cfg(feature = "tls")]
pub mod tls {
    use std::io::{self, Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
    use rustls::{ClientConfig, ClientConnection, Connection, ServerConfig, ServerConnection};

    use super::Duplex;

    fn provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    fn invalid(e: impl std::fmt::Display) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, e.to_string())
    }

    /// A server config from PEM `cert` (chain) and `key`.
    pub fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<ServerConfig>, String> {
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<_, _>>()
            .map_err(|e| format!("server.https cert: {e}"))?;
        if certs.is_empty() {
            return Err("server.https cert: no certificate in the PEM".to_owned());
        }
        let key =
            PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("server.https key: {e}"))?;
        let mut cfg = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| format!("server.https: {e}"))?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    }

    /// Accepts TLS on `tcp` (handshake done before returning).
    pub fn accept(tcp: TcpStream, cfg: &Arc<ServerConfig>) -> io::Result<Duplex> {
        let conn = ServerConnection::new(Arc::clone(cfg)).map_err(invalid)?;
        split(tcp, Connection::Server(conn))
    }

    #[derive(Debug)]
    struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
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
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    fn client_config(insecure: bool) -> io::Result<Arc<ClientConfig>> {
        let builder = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(invalid)?;
        let mut cfg = if insecure {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(provider())))
                .with_no_client_auth()
        } else {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            builder.with_root_certificates(roots).with_no_client_auth()
        };
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    }

    /// Connects TLS over `tcp` to `host`.
    pub fn connect(tcp: TcpStream, host: &str, insecure: bool) -> io::Result<Duplex> {
        let name = ServerName::try_from(host.to_owned()).map_err(invalid)?;
        let conn = ClientConnection::new(client_config(insecure)?, name).map_err(invalid)?;
        split(tcp, Connection::Client(conn))
    }

    struct State {
        conn: Mutex<Connection>,
    }

    fn split(mut tcp: TcpStream, mut conn: Connection) -> io::Result<Duplex> {
        let old = tcp.read_timeout()?;
        tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut tcp)?;
        }
        tcp.set_read_timeout(old)?;
        let state = Arc::new(State {
            conn: Mutex::new(conn),
        });
        Ok(Duplex {
            reader: Box::new(TlsReader {
                state: Arc::clone(&state),
                tcp: tcp.try_clone()?,
                pending: Vec::new(),
                at: 0,
            }),
            writer: Box::new(TlsWriter {
                state,
                tcp: tcp.try_clone()?,
            }),
            tcp,
        })
    }

    struct TlsReader {
        state: Arc<State>,
        tcp: TcpStream,
        /// Ciphertext read from the socket, not yet fed to rustls.
        pending: Vec<u8>,
        at: usize,
    }

    impl Read for TlsReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            loop {
                {
                    let mut c = self.state.conn.lock().unwrap_or_else(|e| e.into_inner());
                    match c.reader().read(buf) {
                        Ok(n) => return Ok(n),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                        Err(e) => return Err(e),
                    }
                    // No plaintext buffered: feed what we have (rustls never refuses input
                    // while its plaintext buffer is empty).
                    if self.at < self.pending.len() {
                        let mut rest = &self.pending[self.at..];
                        let before = rest.len();
                        let n = c.read_tls(&mut rest)?;
                        self.at += before - rest.len();
                        if n == 0 {
                            return Ok(0);
                        }
                        let state = c.process_new_packets().map_err(invalid);
                        while c.wants_write() {
                            c.write_tls(&mut self.tcp)?;
                        }
                        state?;
                        continue;
                    }
                }
                self.pending.resize(16 * 1024 + 256, 0);
                self.at = 0;
                let n = self.tcp.read(&mut self.pending)?;
                self.pending.truncate(n);
                if n == 0 {
                    return Ok(0);
                }
            }
        }
    }

    struct TlsWriter {
        state: Arc<State>,
        tcp: TcpStream,
    }

    impl Write for TlsWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut c = self.state.conn.lock().unwrap_or_else(|e| e.into_inner());
            let n = c.writer().write(buf)?;
            while c.wants_write() {
                c.write_tls(&mut self.tcp)?;
            }
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut c = self.state.conn.lock().unwrap_or_else(|e| e.into_inner());
            c.writer().flush()?;
            while c.wants_write() {
                c.write_tls(&mut self.tcp)?;
            }
            Ok(())
        }
    }
}
