//! Who may use the web app: the password, the tokens paired devices
//! carry, where requests may come from, and how often a password may be
//! tried.
//!
//! - The password is kept as an Argon2id hash, in PHC form, never as
//!   itself. Checking it takes tens of milliseconds by design, so it's
//!   checked on a thread of its own, one try at a time, at most
//!   [`TRIES`] in a burst and one more each [`TRY_EVERY`] after: the
//!   limit is the daemon's, not each address's, as every request through
//!   Tailscale comes from this machine.
//! - A paired device's token is 32 random bytes, which it carries in a
//!   cookie its scripts can't read. The daemon keeps only its SHA-256:
//!   with that much randomness in it, a hash that's slow to make would
//!   add nothing, and a leak of the devices' file gives no tokens away.
//! - A request that changes something, and the WebSocket, must come from
//!   a page of the app's own origin: see [`origin_allowed`].
//! - Through Tailscale, a request must come through the unix socket
//!   `tailscale serve` connects to, with the login it sets: see
//!   [`tailscale_login`]. The socket keeps other users out, but the
//!   user's own programs can reach it, and write any header: what lets a
//!   device in is its token, in its cookie. The login only narrows who
//!   may pair, and says who asks.
//! - Tries of the password, and asks to be allowed from a window, are
//!   limited for each address they come from (see [`Buckets`]), so one
//!   client can't use up everyone's.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use super::conn::Peer;

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};

/// How many passwords may be tried in a burst.
pub(super) const TRIES: u32 = 5;

/// How often another try is allowed, once they're used up.
pub(super) const TRY_EVERY: Duration = Duration::from_mins(1);

/// How many passwords may be tried in a burst from everywhere at once,
/// and how often one more may: about twenty a minute, however many
/// addresses they come from.
pub(super) const ALL_TRIES: u32 = 20;
pub(super) const ALL_TRY_EVERY: Duration = Duration::from_secs(3);

/// What tries from `ip` are counted under: the address itself for IPv4,
/// and its /64 for IPv6, which one subscriber usually has whole, so
/// hopping between its addresses gains nothing.
pub(super) fn address_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return ip.to_string();
            }
            let [a, b, c, d, ..] = ip.segments();
            format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
        }
    }
}

/// The name of the cookie a paired device's token goes in. `__Host-`
/// makes the browser keep it to this origin, over HTTPS, for every path.
pub(super) const COOKIE: &str = "__Host-clackatoo";

/// How long the browser keeps the cookie: as long as browsers allow. The
/// daemon expires an unused device sooner, itself.
const COOKIE_AGE: Duration = Duration::from_hours(400 * 24);

/// The hash of `password` to keep in `web.toml`: Argon2id, with OWASP's
/// recommended costs (19 MiB, two passes) and a random salt, in PHC form.
///
/// # Panics
///
/// If the system has no randomness to give, which it always has.
#[must_use]
pub fn hash_password(password: &str) -> String {
    let mut salt = [0; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .expect("the system gives randomness");
    let salt = argon2::password_hash::SaltString::encode_b64(&salt).expect("16 bytes is a salt");
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("the default costs hash anything")
        .to_string()
}

/// Whether `hash` is a password hash that can be checked against.
///
/// # Errors
///
/// If it isn't an Argon2 hash in PHC form.
pub(super) fn check_hash(hash: &str) -> Result<(), String> {
    let parsed = PasswordHash::new(hash)
        .map_err(|_| "isn't a password hash: `clackatoo web password` writes one".to_owned())?;
    if !parsed.algorithm.as_str().starts_with("argon2") {
        return Err(format!("is a {} hash, not Argon2", parsed.algorithm));
    }
    Ok(())
}

/// Whether `password` is the one `hash` was made from. Slow, by design:
/// never on the event loop. The comparison inside takes the same time
/// however much of it matches.
pub(super) fn password_matches(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// A new device token, as the cookie carries it, and its hash, as the
/// daemon keeps it.
pub(super) fn new_token(random: &SystemRandom) -> (String, [u8; 32]) {
    let mut token = [0; 32];
    random
        .fill(&mut token)
        .expect("the system gives randomness");
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token);
    let hash = token_hash(&text);
    (text, hash)
}

/// The hash of a token, as a cookie carries it.
pub(super) fn token_hash(token: &str) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    digest.as_ref().try_into().expect("SHA-256 is 32 bytes")
}

/// Whether two hashes are the same, taking as long however much of them
/// matches.
pub(super) fn same_hash(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0, |differ, (a, b)| differ | (a ^ b)) == 0
}

/// The `Set-Cookie` header that gives a device `token`.
pub(super) fn set_cookie(token: &str) -> String {
    format!(
        "{COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={}",
        COOKIE_AGE.as_secs()
    )
}

/// Why a request isn't let in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Through Tailscale, it didn't come from `tailscale serve`.
    NotFromTailscale,
    /// Through Tailscale, it came from someone other than the login let
    /// in.
    OtherLogin,
    /// It came from a page of another origin, or said none.
    OtherOrigin,
}

impl Refused {
    /// What to tell the client.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::NotFromTailscale => "only `tailscale serve` on this machine reaches the app",
            Self::OtherLogin => "this Tailscale login isn't let in",
            Self::OtherOrigin => "the request came from a page that isn't the app's",
        }
    }
}

/// Who Tailscale says sent a request that came from `peer` saying
/// `login` in `Tailscale-User-Login`. `tailscale serve` sets the header
/// itself, taking away any the client sent, and connects through the
/// unix socket, so the header is taken from there alone: never from the
/// network, nor from TCP on this machine, which any user's program can
/// reach. The user's own programs, and root, can reach the socket and
/// write the header too, so it's never what lets a device in: a device's
/// token is. A request without it, as from a tagged device or through
/// Funnel, isn't let in.
///
/// # Errors
///
/// If the request didn't come through `tailscale serve`.
pub(crate) fn tailscale_login(peer: Peer, login: Option<&str>) -> Result<String, Refused> {
    if peer != Peer::Local {
        return Err(Refused::NotFromTailscale);
    }
    match login.map(str::trim) {
        Some(login) if !login.is_empty() => Ok(login.to_owned()),
        _ => Err(Refused::NotFromTailscale),
    }
}

/// Whether a request sent to `host`, or as `tailscale serve` says, to
/// `forwarded`, was sent to the app's own name: the one in `allowed`, the
/// origin the settings name, if they name one. A page elsewhere whose
/// name was pointed at this machine (DNS rebinding) is refused.
pub(crate) fn host_allowed(
    allowed: Option<&str>,
    host: Option<&str>,
    forwarded: Option<&str>,
) -> bool {
    let Some(allowed) = allowed else {
        return true;
    };
    let name = allowed.strip_prefix("https://").unwrap_or(allowed);
    [host, forwarded]
        .into_iter()
        .flatten()
        .any(|host| host.eq_ignore_ascii_case(name))
}

/// Whether a request saying it came from a page of `origin` may act: it
/// must be `allowed`, if the settings name one, or else the origin it was
/// sent to, over HTTPS: at `host`, or through Tailscale, at the host
/// `tailscale serve` says it was sent to (`forwarded`). A browser always
/// says, for a WebSocket and for a POST, and a page can't lie about it.
pub(crate) fn origin_allowed(
    origin: Option<&str>,
    allowed: Option<&str>,
    host: Option<&str>,
    forwarded: Option<&str>,
) -> bool {
    let Some(origin) = origin.map(str::to_ascii_lowercase) else {
        return false;
    };
    if let Some(allowed) = allowed {
        return origin == allowed;
    }
    [host, forwarded]
        .into_iter()
        .flatten()
        .any(|host| origin == format!("https://{}", host.to_ascii_lowercase()))
}

/// The most addresses whose tries are kept apart: past it, those whose
/// tries have all come back are forgotten, and if none have, the one
/// least lately tried.
const MOST_KEYS: usize = 4096;

/// How many tries of something are allowed now from each address.
#[derive(Debug)]
pub(super) struct Buckets {
    size: u32,
    every: Duration,
    by_key: HashMap<String, Bucket>,
}

impl Buckets {
    /// A full bucket of `size` for each address, filling one each `every`.
    pub(super) fn new(size: u32, every: Duration) -> Self {
        Self {
            size,
            every,
            by_key: HashMap::new(),
        }
    }

    /// Takes a try for `key`, if one is allowed at `now`, or says how long
    /// until one is.
    pub(super) fn take(&mut self, key: &str, now: Instant) -> Result<(), Duration> {
        if !self.by_key.contains_key(key) && self.by_key.len() >= MOST_KEYS {
            self.by_key.retain(|_, bucket| !bucket.full(now));
            // Never everyone locked out: the oldest goes.
            if self.by_key.len() >= MOST_KEYS
                && let Some(oldest) = (self.by_key.iter())
                    .min_by_key(|(_, bucket)| bucket.tried)
                    .map(|(key, _)| key.clone())
            {
                self.by_key.remove(&oldest);
            }
        }
        (self.by_key.entry(key.to_owned()))
            .or_insert_with(|| Bucket::new(self.size, self.every, now))
            .take(now)
    }
}

/// How many tries of something are allowed now: a few at once, then one
/// each while.
#[derive(Debug)]
pub(super) struct Bucket {
    /// How many it holds when full.
    size: u32,
    /// How often one more is allowed.
    every: Duration,
    /// How many there were at `since`.
    left: u32,
    since: Instant,
    /// When a try was last taken, or asked for.
    tried: Instant,
}

impl Bucket {
    /// A full bucket of `size`, filling one each `every`.
    pub(super) fn new(size: u32, every: Duration, now: Instant) -> Self {
        Self {
            size,
            every,
            left: size,
            since: now,
            tried: now,
        }
    }

    /// Whether every try has come back by `now`.
    fn full(&self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.since);
        let earned = elapsed.as_nanos() / self.every.as_nanos().max(1);
        u128::from(self.left) + earned >= u128::from(self.size)
    }

    /// Takes a try, if one is allowed at `now`, or says how long until
    /// one is.
    pub(super) fn take(&mut self, now: Instant) -> Result<(), Duration> {
        self.tried = now;
        let elapsed = now.saturating_duration_since(self.since);
        let earned =
            u32::try_from(elapsed.as_nanos() / self.every.as_nanos().max(1)).unwrap_or(u32::MAX);
        if earned > 0 {
            self.left = self.left.saturating_add(earned).min(self.size);
            self.since += self.every * earned.min(self.size);
            if self.left == self.size {
                self.since = now;
            }
        }
        if self.left == 0 {
            return Err((self.since + self.every).saturating_duration_since(now));
        }
        self.left -= 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_matches_only_its_own_hash() {
        let hash = hash_password("correct horse");
        assert!(hash.starts_with("$argon2id$"));
        assert!(check_hash(&hash).is_ok());
        assert!(password_matches("correct horse", &hash));
        assert!(!password_matches("correct horsf", &hash));
        assert!(!password_matches("correct horse", "not a hash"));
        assert!(check_hash("hunter2").is_err());
        // Two hashes of one password differ by their salts.
        assert_ne!(hash, hash_password("correct horse"));
    }

    #[test]
    fn tokens_are_long_random_and_kept_only_as_hashes() {
        let random = SystemRandom::new();
        let (token, hash) = new_token(&random);
        let (other, _) = new_token(&random);
        assert_eq!(token.len(), 43, "32 bytes in base64url");
        assert_ne!(token, other);
        assert!(same_hash(&token_hash(&token), &hash));
        assert!(!same_hash(&token_hash(&other), &hash));
        let cookie = set_cookie(&token);
        for part in [
            "__Host-clackatoo=",
            "Secure",
            "HttpOnly",
            "SameSite=Strict",
            "Path=/",
        ] {
            assert!(cookie.contains(part), "{cookie}");
        }
    }

    #[test]
    fn tailscales_word_is_taken_only_through_the_local_socket() {
        assert_eq!(
            tailscale_login(Peer::Local, Some("jack@github")),
            Ok("jack@github".to_owned())
        );
        for address in [
            "127.0.0.1:5000",
            "[::1]:5000",
            "100.64.0.7:5000",
            "192.168.1.20:5000",
        ] {
            let peer = Peer::Tcp(address.parse().unwrap());
            assert_eq!(
                tailscale_login(peer, Some("jack@github")),
                Err(Refused::NotFromTailscale),
                "a header from {address} is anyone's to write, other users' on this machine too"
            );
        }
        assert_eq!(
            tailscale_login(Peer::Local, None),
            Err(Refused::NotFromTailscale)
        );
        assert_eq!(
            tailscale_login(Peer::Local, Some(" ")),
            Err(Refused::NotFromTailscale)
        );
    }

    #[test]
    fn only_the_apps_own_name_is_answered_once_its_named() {
        assert!(host_allowed(None, Some("anything"), None));
        let allowed = Some("https://desk.tail1.ts.net");
        assert!(host_allowed(allowed, Some("desk.tail1.ts.net"), None));
        assert!(host_allowed(
            allowed,
            Some("127.0.0.1"),
            Some("Desk.tail1.ts.net")
        ));
        assert!(!host_allowed(allowed, Some("evil.example"), None));
        assert!(!host_allowed(allowed, None, None));
    }

    #[test]
    fn tries_are_counted_by_address_and_ipv6_by_64() {
        let key = |ip: &str| address_key(ip.parse().unwrap());
        assert_eq!(key("203.0.113.7"), "203.0.113.7");
        assert_ne!(key("203.0.113.7"), key("203.0.113.8"));
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:203.0.113.7"), "203.0.113.7");
    }

    #[test]
    fn a_full_map_lets_go_of_the_oldest_not_everyone() {
        let start = Instant::now();
        let mut buckets = Buckets::new(1, Duration::from_hours(1));
        for n in 0..MOST_KEYS {
            let at = start + Duration::from_millis(u64::try_from(n).unwrap());
            assert!(buckets.take(&n.to_string(), at).is_ok());
        }
        let later = start + Duration::from_secs(10);
        assert!(buckets.take("newcomer", later).is_ok(), "not locked out");
        assert_eq!(buckets.by_key.len(), MOST_KEYS);
        assert!(!buckets.by_key.contains_key("0"), "the oldest went");
        assert!(buckets.take("1", later).is_err(), "the rest are kept");
    }

    #[test]
    fn each_address_has_tries_of_its_own() {
        let start = Instant::now();
        let mut buckets = Buckets::new(2, Duration::from_mins(1));
        assert!(buckets.take("a", start).is_ok());
        assert!(buckets.take("a", start).is_ok());
        assert!(buckets.take("a", start).is_err());
        assert!(buckets.take("b", start).is_ok(), "a's tries aren't b's");
    }

    #[test]
    fn only_the_apps_own_pages_may_act() {
        let allowed =
            |origin, setting, host, forwarded| origin_allowed(origin, setting, host, forwarded);
        let host = Some("desk.tail1.ts.net");
        assert!(allowed(Some("https://desk.tail1.ts.net"), None, host, None));
        assert!(allowed(Some("https://Desk.tail1.ts.net"), None, host, None));
        assert!(!allowed(Some("http://desk.tail1.ts.net"), None, host, None));
        assert!(!allowed(Some("https://evil.example"), None, host, None));
        assert!(!allowed(None, None, host, None));
        assert!(!allowed(Some("null"), None, host, None));
        assert!(allowed(
            Some("https://desk.tail1.ts.net"),
            None,
            Some("127.0.0.1:8765"),
            host
        ));
        let setting = Some("https://example.com:8443");
        assert!(allowed(
            Some("https://example.com:8443"),
            setting,
            host,
            None
        ));
        assert!(!allowed(
            Some("https://desk.tail1.ts.net"),
            setting,
            host,
            None
        ));
    }

    #[test]
    fn tries_come_back_one_at_a_time() {
        let start = Instant::now();
        let mut bucket = Bucket::new(2, Duration::from_mins(1), start);
        assert!(bucket.take(start).is_ok());
        assert!(bucket.take(start).is_ok());
        assert_eq!(bucket.take(start), Err(Duration::from_mins(1)));
        let later = start + Duration::from_secs(61);
        assert!(bucket.take(later).is_ok());
        assert!(bucket.take(later).is_err());
        // Long unused, it's full again, and no fuller.
        let much_later = later + Duration::from_hours(1);
        assert!(bucket.take(much_later).is_ok());
        assert!(bucket.take(much_later).is_ok());
        assert!(bucket.take(much_later).is_err());
    }
}
