//! The transport that reaches another machine.
//!
//! QUIC rather than TCP because the encryption is not optional. A plain socket
//! would carry an agent's screen -- file paths, command output, whatever it has
//! printed -- in clear text across whatever network is between two laptops, and
//! "add TLS later" is how that never happens.
//!
//! There is no certificate authority here, so the server mints a self-signed
//! certificate on startup and the [`Invite`] carries its fingerprint. The
//! watcher trusts that one fingerprint and nothing else, which answers the
//! question actually being asked -- "is this the machine the inviter meant" --
//! rather than the one a CA answers.
//!
//! Holding the certificate is not authorization. Anyone who learns the address
//! can complete a handshake, so the first thing a client sends is the invite's
//! secret, and only a valid, unused, unexpired secret is exchanged for a
//! [`Capability::WatchSession`]. Everything else this API can do stays
//! unreachable -- see [`Capability`].

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use crate::invite::{
    INVITE_LIFETIME, Invite, InviteError, SECRET_LEN, hex, now_unix, secrets_match,
};
use crate::{Capability, Client, ClientError, Handler, server::serve_connection};
use argus_protocol::{ErrorCode, RpcError};

/// Negotiated so that a stray client speaking some other protocol is turned
/// away by the handshake rather than by a confusing parse error later.
const ALPN: &[u8] = b"argus/1";

/// Sent back when the secret is accepted. One byte, because the client needs to
/// tell "refused" from "connected but quiet", and a closed connection alone
/// does not say which.
const ACCEPTED: u8 = 1;
const REFUSED: u8 = 0;

/// How long a peer gets to finish the handshake and present its secret.
///
/// Everything before this point is unauthenticated, so without a deadline a
/// peer that connects and then says nothing holds a task and a connection for
/// as long as it likes, and enough of them exhaust the host. Ten seconds is far
/// more than a loopback or WAN handshake needs.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How many peers may be mid-handshake at once.
///
/// A second line behind the timeout: the timeout bounds how long each
/// unauthenticated peer lasts, this bounds how many there can be. Watchers are
/// invited by hand, so even a small number is never reached in normal use.
const MAX_HANDSHAKES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error("could not generate a certificate for this machine")]
    Certificate(#[from] rcgen::Error),

    #[error("could not configure TLS")]
    Tls(#[from] rustls::Error),

    /// The TLS 1.3 cipher suites QUIC needs are missing from the provider.
    #[error("this build of rustls offers no cipher suite QUIC can use")]
    NoCipherSuite,

    #[error("could not bind {addr}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read the listening address")]
    LocalAddr(#[source] std::io::Error),
}

/// One outstanding invite.
struct Pending {
    session: String,
    expires_unix: u64,
}

/// A QUIC endpoint that serves watchers holding a valid invite.
pub struct QuicListener {
    endpoint: Endpoint,
    fingerprint: String,
    /// Keyed by secret. Entries are removed when used, so an invite works once.
    pending: Arc<Mutex<HashMap<[u8; SECRET_LEN], Pending>>>,
    /// Permits for peers that have not yet proved they were invited.
    handshakes: Arc<tokio::sync::Semaphore>,
}

impl QuicListener {
    /// Bind, minting a fresh certificate for this run.
    ///
    /// The certificate lives and dies with the process. It identifies this
    /// engine to whoever was handed an invite, and nothing longer-lived would
    /// add anything: an invite is valid for minutes, and a key on disk is a key
    /// that can be stolen.
    pub fn bind(addr: SocketAddr) -> Result<Self, QuicError> {
        let certified = rcgen::generate_simple_self_signed(vec!["argus".to_string()])?;
        let cert = CertificateDer::from(certified.cert);
        let fingerprint = hex(&Sha256::digest(&cert));
        let key = PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der());

        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];

        let server = ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(tls).map_err(|_| QuicError::NoCipherSuite)?,
        ));
        let endpoint =
            Endpoint::server(server, addr).map_err(|source| QuicError::Bind { addr, source })?;

        Ok(Self {
            endpoint,
            fingerprint,
            pending: Arc::new(Mutex::new(HashMap::new())),
            handshakes: Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES)),
        })
    }

    /// Where this endpoint is actually listening, with port 0 resolved.
    pub fn local_addr(&self) -> Result<SocketAddr, QuicError> {
        self.endpoint.local_addr().map_err(QuicError::LocalAddr)
    }

    /// SHA-256 of this run's certificate, lowercase hex.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Mint an invite to watch one session.
    ///
    /// The secret comes from the OS, not from any state this process keeps, so
    /// two invites never collide and knowing one says nothing about the next.
    pub fn invite(&self, session: &str) -> Result<Invite, QuicError> {
        let mut secret = [0u8; SECRET_LEN];
        getrandom(&mut secret);
        let expires_unix = now_unix() + INVITE_LIFETIME.as_secs();

        let mut pending = self.pending.lock().expect("invite table poisoned");
        pending.retain(|_, p| p.expires_unix > now_unix());
        pending.insert(
            secret,
            Pending {
                session: session.to_string(),
                expires_unix,
            },
        );

        Ok(Invite {
            addr: self.local_addr()?,
            session: session.to_string(),
            fingerprint: self.fingerprint.clone(),
            secret,
            expires_unix,
        })
    }

    /// Exchange a secret for the capability it was issued with, consuming it.
    fn redeem(&self, secret: &[u8; SECRET_LEN]) -> Result<Capability, InviteError> {
        let mut pending = self.pending.lock().expect("invite table poisoned");

        // Every comparison is constant time, so no answer reveals how much of
        // a guessed secret was right. The scan itself stops at a match, which
        // is fine: that only says a correct secret was found, which the caller
        // is about to be told anyway.
        let found = pending
            .keys()
            .find(|known| secrets_match(known, secret))
            .copied();

        // Pruned here as well as on mint, so a listener that issues one invite
        // and never another does not keep it past its life.
        pending.retain(|key, p| p.expires_unix > now_unix() || found == Some(*key));

        let Some(key) = found else {
            return Err(InviteError::Unknown);
        };
        // Removed whether or not it is still valid: a single-use token that is
        // left behind after a failed attempt can be retried.
        let entry = pending.remove(&key).expect("just found");
        if entry.expires_unix <= now_unix() {
            return Err(InviteError::Expired);
        }
        Ok(Capability::WatchSession { id: entry.session })
    }

    /// Accept watchers until `shutdown` completes.
    ///
    /// Takes an `Arc` so the caller can go on minting invites from the same
    /// listener while it serves -- which is the normal case, since the invite
    /// has to come from the endpoint that will honour it.
    pub async fn serve_forever<H: Handler>(
        self: Arc<Self>,
        handler: Arc<H>,
        shutdown: impl Future<Output = ()>,
    ) {
        tokio::pin!(shutdown);
        let listener = self;

        loop {
            let incoming = tokio::select! {
                incoming = listener.endpoint.accept() => match incoming {
                    Some(incoming) => incoming,
                    None => return,
                },
                () = &mut shutdown => {
                    // Tell anyone connected, rather than letting them wait for
                    // a timeout to work out that the engine went away.
                    listener.endpoint.close(0u32.into(), b"engine stopped");
                    return;
                }
            };

            let handler = Arc::clone(&handler);
            let listener = Arc::clone(&listener);
            // Turned away rather than queued: a peer that cannot be handled
            // now should be told, not left holding a socket while the host is
            // already saturated.
            let Ok(permit) = Arc::clone(&listener.handshakes).try_acquire_owned() else {
                incoming.refuse();
                continue;
            };

            tokio::spawn(async move {
                // One bad peer must never take the listener down with it, so
                // every failure here ends this connection and nothing else.
                let _ = listener.accept_one(incoming, handler, permit).await;
            });
        }
    }

    async fn accept_one<H: Handler>(
        &self,
        incoming: quinn::Incoming,
        handler: Arc<H>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Every step up to and including the secret is on the clock, because
        // every step up to there is unauthenticated.
        let handshake = async {
            let connection = incoming.await?;
            let (send, mut recv) = connection.accept_bi().await?;
            let mut secret = [0u8; SECRET_LEN];
            recv.read_exact(&mut secret).await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((connection, send, recv, secret))
        };
        let (connection, mut send, recv, secret) =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await??;

        let capability = match self.redeem(&secret) {
            Ok(capability) => capability,
            Err(_) => {
                // The same answer for an unknown secret, an expired one and one
                // already used. Which of those it was is exactly what an
                // attacker would like to learn.
                let _ = send.write_all(&[REFUSED]).await;
                let _ = send.finish();
                connection.close(0u32.into(), b"not invited");
                return Ok(());
            }
        };
        send.write_all(&[ACCEPTED]).await?;

        // Released once the peer is past the handshake: the limit exists to
        // bound unauthenticated peers, and this one no longer is.
        drop(permit);

        // From here it is the same request loop the local pipe uses, with the
        // capability doing the containing.
        serve_connection(tokio::io::join(recv, send), handler, capability).await;
        Ok(())
    }
}

/// Random bytes from the operating system.
///
/// `ring` is already in the dependency tree for TLS, so its system random is
/// used rather than adding a crate for one call.
fn getrandom(buf: &mut [u8; SECRET_LEN]) {
    use ring::rand::SecureRandom;
    ring::rand::SystemRandom::new()
        .fill(buf)
        .expect("the OS random source is unavailable");
}

/// Dial the machine an invite points at and present its secret.
///
/// On success the returned [`Client`] speaks the ordinary protocol, except
/// that the engine will refuse anything the invite's capability does not
/// cover -- in practice, `watch` of the one session it names.
pub async fn connect(invite: &Invite) -> Result<Client, ClientError> {
    // Bind whatever local port the OS offers, matching the family of the
    // address being dialled: a v4 socket cannot reach a v6 peer.
    let local: SocketAddr = if invite.addr.is_ipv6() {
        "[::]:0".parse().expect("a literal address")
    } else {
        "0.0.0.0:0".parse().expect("a literal address")
    };
    let mut endpoint = Endpoint::client(local).map_err(quic_io)?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(quic_tls)?
        .dangerous()
        // Not a web PKI situation. There is no authority that has heard of
        // this machine, so the invite's fingerprint is the trust anchor and
        // the only certificate accepted is that exact one.
        .with_custom_certificate_verifier(Arc::new(PinnedCertificate {
            fingerprint: invite.fingerprint.clone(),
            provider,
        }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let config =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls).map_err(|_| {
            quic_tls(rustls::Error::General(
                "no cipher suite QUIC can use".into(),
            ))
        })?));
    endpoint.set_default_client_config(config);

    // The name is not checked against the certificate -- the fingerprint is --
    // but a QUIC handshake needs one, and it has to match what the server put
    // in its certificate.
    let connection = endpoint
        .connect(invite.addr, "argus")
        .map_err(|e| ClientError::NotRunning(format!("{}: {e}", invite.addr)))?
        .await
        .map_err(|e| ClientError::NotRunning(format!("{}: {e}", invite.addr)))?;

    let (mut send, mut recv) = connection.open_bi().await.map_err(quic_io)?;
    send.write_all(&invite.secret).await.map_err(quic_io)?;

    let mut answer = [0u8; 1];
    recv.read_exact(&mut answer).await.map_err(|_| refused())?;
    if answer[0] != ACCEPTED {
        return Err(refused());
    }

    // Both have to outlive the streams -- dropping the endpoint closes
    // everything underneath it -- and both are dropped with the client. A task
    // waiting on `closed()` instead would never finish, because nothing else
    // closes the connection, and would hold the endpoint open for the idle
    // timeout after every watch.
    let transport = Box::new(QuicTransport {
        connection: connection.clone(),
        _endpoint: endpoint,
    });

    Ok(Client::from_stream(
        Box::new(tokio::io::join(recv, send)),
        Some(transport),
    ))
}

/// Closes the connection when the client goes, rather than leaving the peer to
/// discover it at the idle timeout.
struct QuicTransport {
    connection: quinn::Connection,
    _endpoint: Endpoint,
}

impl Drop for QuicTransport {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"watcher done");
    }
}

fn refused() -> ClientError {
    ClientError::Rpc(RpcError::new(
        ErrorCode::Forbidden,
        "the invite was refused: it may be expired, already used, or for another engine"
            .to_string(),
    ))
}

fn quic_io(error: impl std::fmt::Display) -> ClientError {
    ClientError::Io(std::io::Error::other(error.to_string()))
}

fn quic_tls(error: rustls::Error) -> ClientError {
    ClientError::Io(std::io::Error::other(error.to_string()))
}

/// Accepts exactly one certificate: the one whose fingerprint the invite
/// carries.
///
/// This replaces web PKI rather than weakening it. The usual verifier answers
/// "has some authority vouched for this name", which nothing has for a laptop.
/// Pinning answers "is this the machine whose invite I am holding", which is
/// the question that matters, and it is a stricter answer: one certificate,
/// not any certificate a CA would sign.
#[derive(Debug)]
struct PinnedCertificate {
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let presented = hex(&Sha256::digest(end_entity));
        if presented == self.fingerprint {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            // Deliberately not naming either fingerprint: whoever is on the
            // other end does not need to be told what was expected.
            Err(rustls::Error::General(
                "the certificate does not match the invite".into(),
            ))
        }
    }

    /// TLS 1.2 is not offered, so reaching this means something is very wrong.
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    /// Pinning the certificate does not mean skipping the proof that the peer
    /// holds its private key. Without this the fingerprint could simply be
    /// replayed by anyone who had seen the certificate.
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
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener() -> QuicListener {
        QuicListener::bind("127.0.0.1:0".parse().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_fingerprint_is_a_sha256_in_hex() {
        let fingerprint = listener().fingerprint().to_string();
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn two_runs_do_not_share_a_certificate() {
        assert_ne!(listener().fingerprint(), listener().fingerprint());
    }

    #[tokio::test]
    async fn an_invite_is_good_once() {
        let listener = listener();
        let invite = listener.invite("3fa1c2d4").unwrap();

        assert_eq!(
            listener.redeem(&invite.secret),
            Ok(Capability::WatchSession {
                id: "3fa1c2d4".into()
            })
        );
        assert_eq!(
            listener.redeem(&invite.secret),
            Err(InviteError::Unknown),
            "a second use must not work"
        );
    }

    #[tokio::test]
    async fn an_unknown_secret_is_refused() {
        let listener = listener();
        listener.invite("3fa1c2d4").unwrap();
        assert_eq!(
            listener.redeem(&[0u8; SECRET_LEN]),
            Err(InviteError::Unknown)
        );
    }

    #[tokio::test]
    async fn an_expired_invite_is_refused_and_consumed() {
        let listener = listener();
        let invite = listener.invite("3fa1c2d4").unwrap();
        listener
            .pending
            .lock()
            .unwrap()
            .get_mut(&invite.secret)
            .unwrap()
            .expires_unix = now_unix() - 1;

        assert_eq!(listener.redeem(&invite.secret), Err(InviteError::Expired));
        assert_eq!(
            listener.redeem(&invite.secret),
            Err(InviteError::Unknown),
            "an expired invite is still consumed, so it cannot be retried"
        );
    }

    #[tokio::test]
    async fn an_invite_names_the_session_it_was_issued_for() {
        let listener = listener();
        let a = listener.invite("3fa1c2d4").unwrap();
        let b = listener.invite("9b0e1172").unwrap();

        assert_eq!(
            listener.redeem(&b.secret),
            Ok(Capability::WatchSession {
                id: "9b0e1172".into()
            })
        );
        assert_eq!(
            listener.redeem(&a.secret),
            Ok(Capability::WatchSession {
                id: "3fa1c2d4".into()
            })
        );
    }

    #[tokio::test]
    async fn an_invite_carries_this_listener_fingerprint_and_port() {
        let listener = listener();
        let invite = listener.invite("3fa1c2d4").unwrap();
        assert_eq!(invite.fingerprint, listener.fingerprint());
        assert_eq!(invite.addr, listener.local_addr().unwrap());
        assert_ne!(invite.addr.port(), 0, "port 0 should be resolved");
    }
}
