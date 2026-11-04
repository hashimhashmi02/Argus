//! The token that lets one person watch one session from another machine.
//!
//! There is no certificate authority between two developers' laptops, so the
//! invite carries everything the watcher needs to verify what it is talking to
//! and prove it was invited:
//!
//! - **where** — the address to dial
//! - **which session** — so the capability it earns names exactly one
//! - **the certificate fingerprint** — the server proves it holds the matching
//!   key through the TLS handshake, and the watcher trusts that fingerprint and
//!   no other. A CA would answer "is this certificate vouched for"; a
//!   fingerprint answers "is this the machine the inviter meant", which is the
//!   question actually being asked here.
//! - **a secret** — presented after the handshake and exchanged for a
//!   [`Capability`](crate::Capability). Without it, anyone who learned the
//!   address could watch.
//!
//! The secret is the part that must not leak, which is why [`Invite::encode`]
//! produces one opaque string to hand over rather than fields a person might
//! paste into separate boxes, and why [`std::fmt::Debug`] does not print it.

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long an invite is good for.
///
/// Short because handing one over is a deliberate act that happens in seconds,
/// and a token that stays valid all afternoon is a token that outlives the
/// conversation it was meant for.
pub const INVITE_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Bytes of secret. 32 bytes from the OS is far past guessing.
pub const SECRET_LEN: usize = 32;

/// Why an invite string could not be read, or could not be accepted.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InviteError {
    #[error("this is not an Argus invite")]
    NotAnInvite,

    #[error("the invite is damaged or incomplete")]
    Malformed,

    #[error("the invite was issued by a newer version of Argus")]
    UnknownVersion(u32),

    #[error("the invite has expired")]
    Expired,

    #[error("the invite has already been used")]
    AlreadyUsed,

    #[error("the invite is not for this server")]
    Unknown,
}

/// What the inviter hands over and the watcher dials.
#[derive(Clone, PartialEq, Eq)]
pub struct Invite {
    pub addr: SocketAddr,
    pub session: String,
    /// SHA-256 of the server's certificate, lowercase hex.
    pub fingerprint: String,
    /// Proof of invitation. Never logged, never printed.
    pub secret: [u8; SECRET_LEN],
}

// Expiry is deliberately absent. It is the issuing server's business, and a
// client cannot be trusted to enforce its own, so sending one would be
// decoration -- and a field that does not survive `encode` is a field someone
// eventually reads and believes. Callers who want to tell a person how long
// they have can use [`INVITE_LIFETIME`].

/// Bumped if the field layout changes, so an old client says "issued by a newer
/// version" rather than misreading the parts.
const INVITE_VERSION: u32 = 1;
const PREFIX: &str = "argus1:";

impl Invite {
    /// One opaque string, so the secret cannot be separated from the rest of
    /// it and pasted somewhere it does not belong.
    /// The address goes last because it is the only field containing dots --
    /// `127.0.0.1` would otherwise be shredded by the separator. Putting it at
    /// the end lets the parse take a fixed number of fields and treat whatever
    /// remains as the address, dots and all.
    pub fn encode(&self) -> String {
        format!(
            "{PREFIX}{}.{}.{}.{}.{}",
            INVITE_VERSION,
            self.session,
            self.fingerprint,
            hex(&self.secret),
            self.addr,
        )
    }

    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let body = text
            .trim()
            .strip_prefix(PREFIX)
            .ok_or(InviteError::NotAnInvite)?;

        // Four fixed fields, then the address. Bounded so the dots inside an
        // IPv4 address stay where they belong -- getting this wrong is why
        // `127.0.0.1` failed to round trip while `[::1]` did.
        let parts: Vec<&str> = body.splitn(5, '.').collect();
        let [version, session, fingerprint, secret, addr] = parts.as_slice() else {
            return Err(InviteError::Malformed);
        };

        let version: u32 = version.parse().map_err(|_| InviteError::Malformed)?;
        if version != INVITE_VERSION {
            return Err(InviteError::UnknownVersion(version));
        }

        let secret = unhex(secret).ok_or(InviteError::Malformed)?;
        if secret.len() != SECRET_LEN {
            return Err(InviteError::Malformed);
        }
        let mut bytes = [0u8; SECRET_LEN];
        bytes.copy_from_slice(&secret);

        if session.is_empty() || fingerprint.len() != 64 {
            return Err(InviteError::Malformed);
        }

        Ok(Self {
            addr: addr.parse().map_err(|_| InviteError::Malformed)?,
            session: (*session).to_string(),
            fingerprint: (*fingerprint).to_string(),
            secret: bytes,
        })
    }
}

/// Deliberately omits the secret. An invite that reaches a log is an invite
/// that has been given away.
impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("addr", &self.addr)
            .field("session", &self.session)
            .field("fingerprint", &self.fingerprint)
            .field("secret", &"<redacted>")
            .finish()
    }
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Compare two secrets without leaking where they first differ.
///
/// `==` on byte arrays stops at the first mismatch, so the time it takes
/// reveals how much of a guess was right, and that is enough to find the rest
/// one byte at a time.
pub(crate) fn secrets_match(a: &[u8; SECRET_LEN], b: &[u8; SECRET_LEN]) -> bool {
    let mut diff = 0u8;
    for i in 0..SECRET_LEN {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Invite {
        Invite {
            addr: "127.0.0.1:4433".parse().unwrap(),
            session: "3fa1c2d4".into(),
            fingerprint: "a".repeat(64),
            secret: [7u8; SECRET_LEN],
        }
    }

    #[test]
    fn an_invite_survives_a_round_trip() {
        let invite = sample();
        assert_eq!(Invite::decode(&invite.encode()).unwrap(), invite);
    }

    #[test]
    fn every_field_is_carried() {
        // The round trip above only means something if nothing was quietly
        // left out of `encode`. Each field is changed on its own and has to
        // come back changed.
        let base = sample();
        let mut other = base.clone();
        other.addr = "127.0.0.1:9999".parse().unwrap();
        other.session = "9b0e1172".into();
        other.fingerprint = "b".repeat(64);
        other.secret = [9u8; SECRET_LEN];

        let decoded = Invite::decode(&other.encode()).unwrap();
        assert_ne!(decoded.addr, base.addr);
        assert_ne!(decoded.session, base.session);
        assert_ne!(decoded.fingerprint, base.fingerprint);
        assert_ne!(decoded.secret, base.secret);
        assert_eq!(decoded, other);
    }

    #[test]
    fn an_ipv6_invite_survives_a_round_trip() {
        let mut invite = sample();
        invite.addr = "[::1]:4433".parse().unwrap();
        assert_eq!(Invite::decode(&invite.encode()).unwrap(), invite);
    }

    #[test]
    fn surrounding_whitespace_is_forgiven() {
        // It is pasted by hand out of a chat window.
        let text = format!("  {}\n", sample().encode());
        assert_eq!(Invite::decode(&text).unwrap(), sample());
    }

    #[test]
    fn anything_else_is_not_an_invite() {
        assert_eq!(
            Invite::decode("hello").unwrap_err(),
            InviteError::NotAnInvite
        );
        assert_eq!(
            Invite::decode("https://example.com").unwrap_err(),
            InviteError::NotAnInvite
        );
    }

    #[test]
    fn a_damaged_invite_is_refused_rather_than_guessed_at() {
        let good = sample().encode();
        for bad in [
            good.replace("127.0.0.1:4433", "not-an-address"),
            good.replace(&"a".repeat(64), "tooshort"),
            format!("{good}.extra"),
            good[..good.len() - 4].to_string(),
            PREFIX.to_string(),
        ] {
            assert!(
                matches!(
                    Invite::decode(&bad),
                    Err(InviteError::Malformed | InviteError::NotAnInvite)
                ),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn a_future_version_says_so_instead_of_misreading_the_fields() {
        let text = sample().encode().replacen("argus1:1.", "argus1:2.", 1);
        assert_eq!(
            Invite::decode(&text).unwrap_err(),
            InviteError::UnknownVersion(2)
        );
    }

    #[test]
    fn the_secret_is_not_in_the_debug_output() {
        let shown = format!("{:?}", sample());
        assert!(shown.contains("redacted"), "{shown}");
        assert!(!shown.contains(&hex(&[7u8; SECRET_LEN])), "{shown}");
    }

    #[test]
    fn secret_comparison_accepts_only_an_exact_match() {
        let a = [1u8; SECRET_LEN];
        assert!(secrets_match(&a, &a));

        // Differing in the first byte and in the last are both rejected, which
        // is the property an early-exit comparison would leak.
        let mut first = a;
        first[0] ^= 0xff;
        assert!(!secrets_match(&a, &first));

        let mut last = a;
        last[SECRET_LEN - 1] ^= 0x01;
        assert!(!secrets_match(&a, &last));
    }
}
