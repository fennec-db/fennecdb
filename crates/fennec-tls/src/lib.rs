//! # fennec-tls
//!
//! TLS for fennecdb's dials and listeners: a thin layer over rustls with
//! ring for the cryptography and Mozilla's roots (webpki-roots) for the
//! default trust store. The one crate besides fennec-bench that takes
//! dependencies from outside the workspace
//! (docs/design/crates.md#zero-dependencies): fennec-core, fennec-ql,
//! fennec-abi, fennec-wasm and the browser module never reach it, and a
//! crate that does takes it behind its `tls` feature.
//!
//! A dial ([`Client::dial`]) completes the handshake before it hands the
//! stream back, so a certificate refused is refused before a byte of the
//! caller's has gone out. What a dial checks of the certificate is the
//! [`Client`]'s: chain and name ([`Client::new`], [`Client::trusting`]),
//! the chain alone ([`Client::chain_only`], PostgreSQL's `verify-ca`), or
//! nothing ([`Client::unverified`], PostgreSQL's `prefer` and `require`:
//! encrypted against whoever listens, authenticated to no one).
//!
//! No type of rustls's crosses the API: a config is built from DER or PEM
//! bytes or files, a stream is `Read` and `Write`, and a failure is this
//! crate's [`Error`]. The backend can be changed here -- BoringSSL, as Bun
//! takes it, is weighed against rustls -- without touching a caller.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, ServerConfig,
    ServerConnection, SignatureScheme,
};
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// ring's provider, named rather than left to rustls's crate features: in a
/// build where another crate turned on aws-lc-rs too (fennec-bench's
/// clients), `ClientConfig::builder()` cannot pick one and panics.
fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())))
}

/// Why a config could not be built or a handshake not completed, in this
/// crate's words: the backend's own error never crosses the API.
#[derive(Debug)]
pub enum Error {
    /// The socket failed, the peer ended the connection, or a file could
    /// not be read.
    Io(io::Error),
    /// The peer's certificate was refused -- its chain, its name, its
    /// dates -- before anything of the caller's was sent.
    Certificate(String),
    /// The handshake failed otherwise: an alert from the peer, no protocol
    /// version or ALPN protocol in common, bytes that are not TLS.
    Handshake(String),
    /// A certificate, key or name given to build a config or to dial is
    /// unusable.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Certificate(m) => write!(f, "certificate refused: {m}"),
            Error::Handshake(m) => write!(f, "TLS handshake failed: {m}"),
            Error::Config(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> io::Error {
        match e {
            Error::Io(e) => e,
            Error::Config(m) => io::Error::new(ErrorKind::InvalidInput, m),
            e => io::Error::new(ErrorKind::InvalidData, e.to_string()),
        }
    }
}

impl Error {
    /// What a handshake's I/O failed with, sorted: rustls hands its own
    /// error inside an `InvalidData`, and a certificate refused is told
    /// apart from the rest.
    fn of_handshake(e: io::Error, peer: &str) -> Error {
        let Some(tls) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) else {
            return Error::Io(io::Error::new(e.kind(), format!("TLS with {peer}: {e}")));
        };
        match tls {
            rustls::Error::InvalidCertificate(c) => Error::Certificate(format!(
                "{peer}: {}",
                rustls::Error::InvalidCertificate(c.clone())
            )),
            other => Error::Handshake(format!("{peer}: {other}")),
        }
    }
}

fn config(e: impl fmt::Display) -> Error {
    Error::Config(e.to_string())
}

fn invalid(e: impl fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, e.to_string())
}

/// A file's bytes, an error naming it.
fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path)
        .map_err(|e| Error::Io(io::Error::new(e.kind(), format!("{}: {e}", path.display()))))
}

/// The certificates of a PEM text, each as its DER bytes: what
/// [`Client::trusting`] takes from a CA file such as PostgreSQL's
/// `sslrootcert`.
pub fn pem_certs(pem: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    CertificateDer::pem_slice_iter(pem)
        .map(|c| c.map(|c| c.to_vec()))
        .collect::<Result<_, _>>()
        .map_err(|e| config(format!("not a PEM certificate: {e}")))
}

// ------------------------------------------------------------------ client

/// What a dial checks of the certificate it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// The chain up to a trusted root, and the name dialed.
    Name,
    /// The chain alone.
    Chain,
    /// Nothing: encryption against a passive listener, no more.
    Nothing,
}

/// Whom a dial trusts and what it checks, and the protocols it offers by
/// ALPN. Built once and dialed many times: its session cache is shared by
/// its dials.
#[derive(Clone)]
pub struct Client {
    config: Arc<ClientConfig>,
    roots: Arc<RootCertStore>,
    check: Check,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("roots", &self.roots.len())
            .field("check", &self.check)
            .field("alpn", &self.config.alpn_protocols)
            .finish()
    }
}

impl Default for Client {
    fn default() -> Client {
        Client::new()
    }
}

impl Client {
    /// Mozilla's roots, compiled in (webpki-roots), the chain and the name
    /// dialed both checked. Compiled in rather than read from the system:
    /// a static binary in a scratch image has no store to read.
    pub fn new() -> Client {
        Client::with_roots(RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
    }

    /// Only the CAs given, each a certificate's DER bytes ([`pem_certs`]
    /// reads them from a PEM file); the chain and the name dialed both
    /// checked. None given, or one that does not parse, is an error.
    pub fn trusting<C: AsRef<[u8]>>(cas: &[C]) -> Result<Client, Error> {
        let mut roots = RootCertStore::empty();
        for der in cas {
            roots
                .add(CertificateDer::from(der.as_ref().to_vec()))
                .map_err(|e| config(format!("not a CA certificate: {e}")))?;
        }
        if roots.is_empty() {
            return Err(config("no CA certificate to trust"));
        }
        Ok(Client::with_roots(roots))
    }

    /// [`Client::trusting`] the certificates of a PEM file, such as
    /// PostgreSQL's `sslrootcert`; an error names the file.
    pub fn trusting_pem_file(path: &Path) -> Result<Client, Error> {
        let pem = read_file(path)?;
        pem_certs(&pem)
            .and_then(|cas| Client::trusting(&cas))
            .map_err(|e| config(format!("{}: {e}", path.display())))
    }

    /// Any certificate, unchecked: the connection is encrypted against a
    /// passive listener, and anyone in the path can still stand in for the
    /// server. PostgreSQL's `prefer` and `require`, which libpq checks
    /// nothing under either.
    pub fn unverified() -> Client {
        let mut c = Client::with_roots(RootCertStore::empty());
        c.check = Check::Nothing;
        c.set_verifier(Arc::new(Unverified(provider())));
        c
    }

    /// The chain checked up to these roots, the name not: PostgreSQL's
    /// `verify-ca`, for a private CA that signs only its own servers.
    pub fn chain_only(mut self) -> Client {
        if self.check == Check::Name {
            self.check = Check::Chain;
            let v = ChainOnly {
                roots: Arc::clone(&self.roots),
                provider: provider(),
            };
            self.set_verifier(Arc::new(v));
        }
        self
    }

    /// The protocols offered by ALPN, the first preferred: `b"http/1.1"`
    /// for HTTP, none (the default) for PostgreSQL, whose servers before 17
    /// take no ALPN.
    pub fn alpn(mut self, protocols: &[&[u8]]) -> Client {
        Arc::make_mut(&mut self.config).alpn_protocols =
            protocols.iter().map(|p| p.to_vec()).collect();
        self
    }

    fn with_roots(roots: RootCertStore) -> Client {
        let roots = Arc::new(roots);
        let config = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("ring's provider speaks TLS 1.2 and 1.3")
            .with_root_certificates(Arc::clone(&roots))
            .with_no_client_auth();
        Client {
            config: Arc::new(config),
            roots,
            check: Check::Name,
        }
    }

    fn set_verifier(&mut self, v: Arc<dyn ServerCertVerifier>) {
        Arc::make_mut(&mut self.config)
            .dangerous()
            .set_certificate_verifier(v);
    }

    /// TLS over `sock`, the handshake completed before it returns: a
    /// refused certificate, a handshake that fails or a peer that ends it is
    /// an error with nothing of the caller's sent. `server_name` is the name
    /// dialed, a host name or an address, sent as SNI and held to the
    /// certificate unless the client checks less.
    pub fn dial(&self, mut sock: TcpStream, server_name: &str) -> Result<TlsStream, Error> {
        let name = self.server_name(&sock, server_name)?;
        let mut conn = ClientConnection::new(Arc::clone(&self.config), name)
            .map_err(|e| config(format!("TLS with {server_name}: {e}")))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)
                .map_err(|e| Error::of_handshake(e, server_name))?;
        }
        Ok(TlsStream {
            conn: Conn::Client(conn),
            sock,
        })
    }

    /// The name as rustls takes it. One it takes as neither a host name
    /// nor an address is an error where the name is checked, and the peer's
    /// address where it is not, which sends no SNI, as libpq sends none for
    /// an address. (An underscore is a host name's: Docker's names hold
    /// them.)
    fn server_name(&self, sock: &TcpStream, name: &str) -> Result<ServerName<'static>, Error> {
        let bare = name
            .strip_prefix('[')
            .and_then(|n| n.strip_suffix(']'))
            .unwrap_or(name);
        match ServerName::try_from(bare.to_string()) {
            Ok(n) => Ok(n),
            Err(_) if self.check != Check::Name => Ok(ServerName::from(sock.peer_addr()?.ip())),
            Err(e) => Err(config(format!(
                "`{name}` is not a name a certificate can be checked against: {e}"
            ))),
        }
    }
}

/// The chain up to the roots, the name not looked at (`verify-ca`). The
/// handshake's signatures are checked as rustls checks them.
#[derive(Debug)]
struct ChainOnly {
    roots: Arc<RootCertStore>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for ChainOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let cert = ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &cert,
            &self.roots,
            intermediates,
            now,
            self.provider.signature_verification_algorithms.all,
        )?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Any certificate (`prefer`, `require`). The handshake's signatures are
/// still checked against the certificate shown, so the key exchange is the
/// shown key's holder's, whoever that is.
#[derive(Debug)]
struct Unverified(Arc<CryptoProvider>);

impl ServerCertVerifier for Unverified {
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
        verify_tls12_signature(
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
        verify_tls13_signature(
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

// ------------------------------------------------------------------ server

/// A certificate chain and its key, served to whoever connects, and the
/// protocols taken by ALPN. No client certificate is asked for.
#[derive(Clone)]
pub struct Server {
    config: Arc<ServerConfig>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("alpn", &self.config.alpn_protocols)
            .finish()
    }
}

impl Server {
    /// A chain (the server's certificate first, then any intermediates) and
    /// its private key (PKCS#8, PKCS#1 or SEC1), each a PEM text.
    pub fn from_pem(chain: &[u8], key: &[u8]) -> Result<Server, Error> {
        let chain = CertificateDer::pem_slice_iter(chain)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| config(format!("the certificate is not PEM: {e}")))?;
        if chain.is_empty() {
            return Err(config("no certificate in the certificate's PEM"));
        }
        let key = PrivateKeyDer::from_pem_slice(key)
            .map_err(|e| config(format!("no private key in the key's PEM: {e}")))?;
        let built = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("ring's provider speaks TLS 1.2 and 1.3")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| config(format!("the certificate and key do not serve: {e}")))?;
        Ok(Server {
            config: Arc::new(built),
        })
    }

    /// [`Server::from_pem`] from two files, an error naming the file it
    /// could not read or use.
    pub fn from_pem_files(chain: &Path, key: &Path) -> Result<Server, Error> {
        Server::from_pem(&read_file(chain)?, &read_file(key)?)
            .map_err(|e| config(format!("{} and {}: {e}", chain.display(), key.display())))
    }

    /// The protocols taken by ALPN, the server's order deciding: a client
    /// offering protocols, none of them these, is refused.
    pub fn alpn(mut self, protocols: &[&[u8]]) -> Server {
        Arc::make_mut(&mut self.config).alpn_protocols =
            protocols.iter().map(|p| p.to_vec()).collect();
        self
    }

    /// TLS over an accepted `sock`, the handshake completed before it
    /// returns.
    pub fn accept(&self, mut sock: TcpStream) -> Result<TlsStream, Error> {
        let mut conn = ServerConnection::new(Arc::clone(&self.config)).map_err(config)?;
        let peer = sock
            .peer_addr()
            .map_or_else(|_| "the client".to_string(), |a| a.to_string());
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)
                .map_err(|e| Error::of_handshake(e, &peer))?;
        }
        Ok(TlsStream {
            conn: Conn::Server(conn),
            sock,
        })
    }
}

// ------------------------------------------------------------------ stream

enum Conn {
    Client(ClientConnection),
    Server(ServerConnection),
}

/// One side's connection, whichever side, as a `rustls::Stream` over the
/// socket.
macro_rules! with_stream {
    ($self:ident, $s:ident => $e:expr) => {
        match &mut $self.conn {
            Conn::Client(c) => {
                let mut $s = rustls::Stream::new(c, &mut $self.sock);
                $e
            }
            Conn::Server(c) => {
                let mut $s = rustls::Stream::new(c, &mut $self.sock);
                $e
            }
        }
    };
}

/// A connection whose handshake is done: plaintext in, plaintext out.
pub struct TlsStream {
    conn: Conn,
    sock: TcpStream,
}

impl fmt::Debug for TlsStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsStream")
            .field("sock", &self.sock)
            .field("alpn", &self.alpn_protocol())
            .finish()
    }
}

impl TlsStream {
    /// The socket beneath, for its addresses and options. Reading or
    /// writing it directly would break the stream.
    pub fn get_ref(&self) -> &TcpStream {
        &self.sock
    }

    /// The protocol ALPN settled on, if any.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        match &self.conn {
            Conn::Client(c) => c.alpn_protocol(),
            Conn::Server(c) => c.alpn_protocol(),
        }
    }

    /// Whether plaintext is ready to read, waiting up to `wait` for it:
    /// `false` when none arrived, an `UnexpectedEof` error when the peer
    /// closed. A wait that runs out never leaves a record half read -- the
    /// part that came stays in rustls's buffer -- and a record that carries
    /// no plaintext (a session ticket, a key update) is taken in and waited
    /// past, so a read after `true` finds bytes, where a peek at the socket
    /// would have woken for a record holding none and left the read to wait
    /// out the socket's own timeout.
    pub fn readable(&mut self, wait: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + wait;
        loop {
            let state = match &mut self.conn {
                Conn::Client(c) => c.process_new_packets(),
                Conn::Server(c) => c.process_new_packets(),
            };
            let state = match state {
                Ok(s) => s,
                Err(e) => {
                    // The alert rustls queued for the peer, then the error.
                    let _ = self.flush_tls();
                    return Err(invalid(e));
                }
            };
            if state.plaintext_bytes_to_read() > 0 {
                return Ok(true);
            }
            if state.peer_has_closed() {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "the peer closed the connection",
                ));
            }
            // What a post-handshake message asked to have sent (a key
            // update's answer) goes out before the wait.
            self.flush_tls()?;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(false);
            }
            let before = self.sock.read_timeout()?;
            // A zero timeout is refused by the socket, and means "forever"
            // to the kernel.
            self.sock
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
            let got = match &mut self.conn {
                Conn::Client(c) => c.read_tls(&mut self.sock),
                Conn::Server(c) => c.read_tls(&mut self.sock),
            };
            self.sock.set_read_timeout(before)?;
            match got {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "the peer closed the connection",
                    ))
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(false)
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Everything rustls holds to send, sent.
    fn flush_tls(&mut self) -> io::Result<()> {
        match &mut self.conn {
            Conn::Client(c) => {
                while c.wants_write() {
                    c.write_tls(&mut self.sock)?;
                }
            }
            Conn::Server(c) => {
                while c.wants_write() {
                    c.write_tls(&mut self.sock)?;
                }
            }
        }
        Ok(())
    }
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        with_stream!(self, s => s.read(buf))
    }
}

impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        with_stream!(self, s => s.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        with_stream!(self, s => s.flush())
    }
}

#[cfg(test)]
mod tests {
    //! A private CA made as each test runs (rcgen, this crate's dev-dependency
    //! alone), a server on a port of its own, and the dials it is held to.

    use super::*;
    use rcgen::{
        date_time_ymd, BasicConstraints, Certificate, CertificateParams, DnType,
        ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    };
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    struct Ca {
        cert: Certificate,
        key: KeyPair,
    }

    fn ca(name: &str) -> Ca {
        let key = KeyPair::generate().unwrap();
        let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.distinguished_name.push(DnType::CommonName, name);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        p.not_before = date_time_ymd(2026, 1, 1);
        p.not_after = date_time_ymd(2126, 1, 1);
        Ca {
            cert: p.self_signed(&key).unwrap(),
            key,
        }
    }

    /// A server certificate `ca` signs for `names`: its PEM and its key's.
    fn leaf(ca: &Ca, names: &[&str]) -> (String, String) {
        let key = KeyPair::generate().unwrap();
        let mut p = CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
            .unwrap();
        p.distinguished_name.push(DnType::CommonName, names[0]);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.not_before = date_time_ymd(2026, 1, 1);
        p.not_after = date_time_ymd(2126, 1, 1);
        let cert = p.signed_by(&key, &ca.cert, &ca.key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn server_for(ca: &Ca, names: &[&str]) -> Server {
        let (chain, key) = leaf(ca, names);
        Server::from_pem(chain.as_bytes(), key.as_bytes()).unwrap()
    }

    /// One connection served: the handshake, then four bytes read and `reply`
    /// written. What came over TLS, or how the handshake ended, on the channel.
    fn serve(
        server: Server,
        reply: &'static [u8],
    ) -> (u16, mpsc::Receiver<Result<Vec<u8>, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut s = match server.accept(sock) {
                Ok(s) => s,
                Err(e) => return tx.send(Err(e.to_string())).unwrap(),
            };
            let mut got = [0u8; 4];
            let r = s.read_exact(&mut got).map_err(|e| e.to_string());
            if r.is_ok() {
                s.write_all(reply).unwrap();
                s.flush().unwrap();
            }
            tx.send(r.map(|()| got.to_vec())).unwrap();
        });
        (port, rx)
    }

    fn dial(client: &Client, port: u16, name: &str) -> Result<TlsStream, Error> {
        client.dial(TcpStream::connect(("127.0.0.1", port)).unwrap(), name)
    }

    fn trusting(ca: &Ca) -> Client {
        Client::trusting(&[ca.cert.der()]).unwrap()
    }

    fn ping(s: &mut TlsStream) -> Vec<u8> {
        s.write_all(b"ping").unwrap();
        s.flush().unwrap();
        let mut got = [0u8; 4];
        s.read_exact(&mut got).unwrap();
        got.to_vec()
    }

    #[test]
    fn a_good_handshake_carries_bytes_both_ways() {
        let ca = ca("fennec test CA");
        let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let mut s = dial(&trusting(&ca), port, "localhost").unwrap();
        assert_eq!(ping(&mut s), b"pong");
        assert_eq!(served.recv().unwrap().unwrap(), b"ping");
        assert_eq!(s.alpn_protocol(), None, "no ALPN unless asked for");
    }

    #[test]
    fn alpn_is_settled_per_use() {
        let ca = ca("fennec test CA");
        let server = server_for(&ca, &["localhost"]).alpn(&[b"http/1.1"]);
        let (port, served) = serve(server, b"pong");
        let mut s = dial(
            &trusting(&ca).alpn(&[b"h2", b"http/1.1"]),
            port,
            "localhost",
        )
        .unwrap();
        assert_eq!(s.alpn_protocol(), Some(&b"http/1.1"[..]));
        assert_eq!(ping(&mut s), b"pong");
        served.recv().unwrap().unwrap();
    }

    /// The name dialed is not the certificate's: refused in the dial, and the
    /// server never read a byte of the caller's.
    #[test]
    fn a_name_the_certificate_does_not_hold_is_refused_before_a_byte_is_sent() {
        let ca = ca("fennec test CA");
        let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let e = dial(&trusting(&ca), port, "127.0.0.1").unwrap_err();
        assert!(
            matches!(e, Error::Certificate(_)) && e.to_string().contains("not valid for name"),
            "{e}"
        );
        let at_server = served.recv().unwrap().unwrap_err();
        assert!(at_server.contains("alert"), "{at_server}");
    }

    #[test]
    fn an_unknown_issuer_is_refused_before_a_byte_is_sent() {
        let (ca, other) = (ca("fennec test CA"), ca("another CA"));
        let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let e = dial(&trusting(&other), port, "localhost").unwrap_err();
        assert!(
            matches!(e, Error::Certificate(_)) && e.to_string().contains("UnknownIssuer"),
            "{e}"
        );
        let at_server = served.recv().unwrap().unwrap_err();
        assert!(at_server.contains("alert"), "{at_server}");
    }

    /// Mozilla's roots, which the default client trusts, never signed a CA
    /// made a moment ago.
    #[test]
    fn the_default_roots_refuse_a_private_ca() {
        assert!(
            Client::new().roots.len() > 100,
            "webpki-roots is compiled in"
        );
        let ca = ca("fennec test CA");
        let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let e = dial(&Client::new(), port, "localhost").unwrap_err();
        assert!(e.to_string().contains("UnknownIssuer"), "{e}");
        served.recv().unwrap().unwrap_err();
    }

    /// `verify-ca`: the chain is checked, the name is not.
    #[test]
    fn chain_only_takes_any_name_under_its_ca_and_nothing_else() {
        let (ca, other) = (ca("fennec test CA"), ca("another CA"));
        let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let mut s = dial(&trusting(&ca).chain_only(), port, "127.0.0.1").unwrap();
        assert_eq!(ping(&mut s), b"pong");
        served.recv().unwrap().unwrap();

        let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let e = dial(&trusting(&other).chain_only(), port, "localhost").unwrap_err();
        assert!(e.to_string().contains("UnknownIssuer"), "{e}");
        served.recv().unwrap().unwrap_err();
    }

    /// `prefer` and `require`: any certificate, any name, a name no
    /// certificate could hold included.
    #[test]
    fn unverified_takes_any_certificate() {
        let ca = ca("fennec test CA");
        for name in [
            "localhost",
            "db.example",
            "127.0.0.1",
            "a_container",
            "no such host",
        ] {
            let (port, served) = serve(server_for(&ca, &["localhost"]), b"pong");
            let mut s = dial(&Client::unverified(), port, name).unwrap();
            assert_eq!(ping(&mut s), b"pong", "{name}");
            served.recv().unwrap().unwrap();
        }
        let ca = self::ca("fennec test CA");
        let (port, _served) = serve(server_for(&ca, &["localhost"]), b"pong");
        let e = dial(&trusting(&ca), port, "no such host").unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{e}");
    }

    #[test]
    fn trusting_nothing_or_something_else_is_an_error() {
        assert!(Client::trusting::<&[u8]>(&[]).is_err());
        assert!(Client::trusting(&[b"not a certificate".as_slice()]).is_err());
        let ca = ca("fennec test CA");
        let ders = pem_certs(ca.cert.pem().as_bytes()).unwrap();
        assert_eq!(ders, vec![ca.cert.der().to_vec()]);
        assert!(pem_certs(b"").unwrap().is_empty());
    }

    #[test]
    fn a_server_is_read_from_its_pem_files() {
        let dir = std::env::temp_dir().join(format!("fennec-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ca = ca("fennec test CA");
        let (chain, key) = leaf(&ca, &["localhost"]);
        let (chain_at, key_at) = (dir.join("server.pem"), dir.join("server.key"));
        std::fs::write(&chain_at, &chain).unwrap();
        std::fs::write(&key_at, &key).unwrap();

        let server = Server::from_pem_files(&chain_at, &key_at).unwrap();
        let (port, served) = serve(server, b"pong");
        let mut s = dial(&trusting(&ca), port, "localhost").unwrap();
        assert_eq!(ping(&mut s), b"pong");
        served.recv().unwrap().unwrap();

        let missing = dir.join("missing.key");
        let e = Server::from_pem_files(&chain_at, &missing).unwrap_err();
        assert!(e.to_string().contains("missing.key"), "{e}");
        // The certificate where the key should be: no key in it.
        let e = Server::from_pem_files(&chain_at, &chain_at).unwrap_err();
        assert!(e.to_string().contains("private key"), "{e}");
        // A key that is not the certificate's.
        let (_, stranger) = leaf(&ca, &["localhost"]);
        let e = Server::from_pem(chain.as_bytes(), stranger.as_bytes()).unwrap_err();
        assert!(e.to_string().contains("do not serve"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The wait the PostgreSQL follower makes for its next message: none in
    /// time is `false` with nothing lost, a record is `true`, an end an error.
    #[test]
    fn readable_waits_for_plaintext_and_tells_an_end() {
        let ca = ca("fennec test CA");
        let server = server_for(&ca, &["localhost"]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (go, wait) = mpsc::channel::<()>();
        let served = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let mut s = server.accept(sock).unwrap();
            wait.recv().unwrap();
            s.write_all(b"x").unwrap();
            s.flush().unwrap();
            wait.recv().unwrap();
            // Dropped: the socket closes.
        });
        let mut s = dial(&trusting(&ca), port, "localhost").unwrap();
        let before = s.get_ref().read_timeout().unwrap();
        assert!(!s.readable(Duration::from_millis(50)).unwrap());
        assert_eq!(
            s.get_ref().read_timeout().unwrap(),
            before,
            "the socket's timeout is put back"
        );
        go.send(()).unwrap();
        assert!(s.readable(Duration::from_secs(10)).unwrap());
        let mut one = [0u8; 1];
        s.read_exact(&mut one).unwrap();
        assert_eq!(&one, b"x");
        go.send(()).unwrap();
        served.join().unwrap();
        let e = s.readable(Duration::from_secs(10)).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
    }

    /// The certificates fennec-wire's tests serve and trust, in
    /// `crates/fennec-wire/tests/certs`: made here, so rcgen stays this crate's
    /// dev-dependency alone. `FENNEC_WRITE_CERTS=1 cargo test -p fennec-tls
    /// wire_certs` writes them again (valid 2026 to 2126); otherwise this holds
    /// them to what they are for -- the server's chain is good for `localhost`
    /// under `ca.pem` and unknown under `other-ca.pem`.
    #[test]
    fn wire_certs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fennec-wire/tests/certs");
        if std::env::var_os("FENNEC_WRITE_CERTS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            let (ca, other) = (ca("fennec test CA"), ca("another CA"));
            let (chain, key) = leaf(&ca, &["localhost"]);
            std::fs::write(dir.join("ca.pem"), ca.cert.pem()).unwrap();
            std::fs::write(dir.join("other-ca.pem"), other.cert.pem()).unwrap();
            std::fs::write(dir.join("server.pem"), chain).unwrap();
            std::fs::write(dir.join("server.key"), key).unwrap();
        }
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
        let server = Server::from_pem(&read("server.pem"), &read("server.key")).unwrap();
        let ca = Client::trusting(&pem_certs(&read("ca.pem")).unwrap()).unwrap();
        let other = Client::trusting(&pem_certs(&read("other-ca.pem")).unwrap()).unwrap();

        let (port, served) = serve(server.clone(), b"pong");
        let mut s = dial(&ca, port, "localhost").unwrap();
        assert_eq!(ping(&mut s), b"pong");
        served.recv().unwrap().unwrap();

        let (port, served) = serve(server, b"pong");
        let e = dial(&other, port, "localhost").unwrap_err();
        assert!(e.to_string().contains("UnknownIssuer"), "{e}");
        served.recv().unwrap().unwrap_err();
    }
}
