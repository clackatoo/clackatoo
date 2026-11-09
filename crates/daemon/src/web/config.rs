//! The web app's settings: `daemon.web` in the daemon's config (see
//! [`crate::settings`]), read when the daemon starts or upgrades.
//!
//! ```lua
//! -- Off unless this says otherwise.
//! daemon.web.mode = "tailscale"        -- or "public", or "off"
//! -- tailscale: the unix socket `tailscale serve` connects to, which only
//! -- you and root can reach; by default, web.sock in the web app's data.
//! daemon.web.socket = "/home/you/.local/share/clackatoo/web/web.sock"
//! -- tailscale: the Tailscale login allowed in; else the first device's,
//! -- which only a window can let in.
//! daemon.web.login = "you@github"
//! -- public: where to listen, and the certificate to show, read again
//! -- when its files change.
//! daemon.web.address = "0.0.0.0"
//! daemon.web.port = 8765
//! daemon.web.certificate = "/etc/letsencrypt/live/example.com/fullchain.pem"
//! daemon.web.key = "/etc/letsencrypt/live/example.com/privkey.pem"
//! -- The address the app is reached at: requests for any other name, or
//! -- from any other page, are refused. Required with public.
//! daemon.web.origin = "https://example.com:8765"
//! -- A device unused this long is unpaired.
//! daemon.web.expire_days = 90
//! -- Ask for the password again after this many days; 0 never does.
//! daemon.web.password_days = 0
//! -- Whether a device may ask to be allowed from a window: by default
//! -- with tailscale, not with public.
//! daemon.web.ask = true
//! -- Wrong passwords in a row before pairing with the password locks,
//! -- from 1 to 10000.
//! daemon.web.lock_after = 50
//! ```
//!
//! The password is never in the config, which people keep in git: `clackatoo
//! web password` keeps its Argon2id hash in `web-password` in the state
//! directory (see [`crate::settings::PASSWORD`]). Optional with
//! tailscale, required with public.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use super::Asset;

/// The port the web app listens on in public, unless told otherwise.
pub const DEFAULT_PORT: u16 = 8765;

/// How long a device goes unused before it's unpaired, unless told
/// otherwise.
pub const DEFAULT_EXPIRY: Duration = Duration::from_hours(90 * 24);

/// How many wrong passwords in a row lock pairing with the password,
/// unless told otherwise.
pub const LOCK_AFTER: u32 = 50;

/// The unix socket's name in the web app's data directory, unless told
/// otherwise.
pub const SOCKET: &str = "web.sock";

/// How the web app is reached, and so whom it trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebMode {
    /// Through `tailscale serve`, which gives it HTTPS, and says who's
    /// asking in headers it sets itself: it listens on a unix socket kept
    /// to the user, and takes those headers from there alone. A device's
    /// token, in its cookie, is still what lets it in.
    Tailscale,
    /// Straight from the network, over TLS with the user's own
    /// certificate. A password is required.
    Public {
        /// The certificate chain's PEM file.
        certificate: PathBuf,
        /// Its private key's PEM file.
        key: PathBuf,
    },
}

/// The web app's settings.
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// How it's reached.
    pub mode: WebMode,
    /// For public: where it listens.
    pub address: SocketAddr,
    /// For Tailscale: the unix socket `tailscale serve` connects to.
    pub socket: PathBuf,
    /// For Tailscale: the only login let in. Without one, the login of
    /// the first device paired is the only one, and only a window can
    /// let that device in.
    pub login: Option<String>,
    /// The password, as an Argon2id hash in PHC form, if there's one.
    pub password: Option<String>,
    /// The origin the app is reached at, such as
    /// `https://example.com:8765`: a request for any other name, or from a
    /// page anywhere else, is refused. Without one, a page must be of the
    /// origin the browser asked for.
    pub origin: Option<String>,
    /// How long a device can go unused before it's unpaired.
    pub expire_after: Duration,
    /// How long a device's password lasts before it's asked for again,
    /// if it's asked for again at all.
    pub password_every: Option<Duration>,
    /// Whether a device may ask to be allowed from a window.
    pub ask: bool,
    /// Where devices and the key notifications are signed with are kept.
    pub data: PathBuf,
    /// What's served: the app's files.
    pub assets: &'static [Asset],
    /// How long a request has to arrive whole, from its first byte, and
    /// TLS to shake hands.
    pub head_wait: Duration,
    /// How often the certificate's files are looked at, to read them again
    /// if they've changed, as when they're renewed.
    pub reload_every: Duration,
    /// How many wrong passwords in a row lock pairing with the password,
    /// until a window unlocks it.
    pub lock_after: u32,
    /// The most TLS handshakes worked on in one turn of the event loop:
    /// the rest wait for the next. At least 1.
    pub handshakes_at_once: usize,
    /// The most connections taken from the listener in one turn of the
    /// event loop, and a quarter of the most refused: the rest wait for
    /// the next. At least 1.
    pub accepts_at_once: usize,
}

impl WebConfig {
    /// The settings for `mode` with every other one at its default,
    /// keeping what it keeps in `data`.
    #[must_use]
    pub fn new(mode: WebMode, data: PathBuf) -> Self {
        let tailscale = mode == WebMode::Tailscale;
        Self {
            mode,
            address: SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
            socket: data.join(SOCKET),
            login: None,
            password: None,
            origin: None,
            expire_after: DEFAULT_EXPIRY,
            password_every: None,
            // On the open network, a stranger could keep asking windows.
            ask: tailscale,
            data,
            assets: &[],
            head_wait: super::conn::HEAD_WAIT,
            reload_every: Duration::from_mins(1),
            lock_after: LOCK_AFTER,
            handshakes_at_once: super::conn::HANDSHAKES_AT_ONCE,
            accepts_at_once: super::conn::ACCEPTS_AT_ONCE,
        }
    }

    /// Whether it's reached through Tailscale.
    pub(crate) fn tailscale(&self) -> bool {
        self.mode == WebMode::Tailscale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    /// The web app `text`, the daemon's config, sets up, with the
    /// password's hash `password`; or what's wrong, as the config says it
    /// or as it's off.
    fn web(text: &str, password: Option<&str>) -> Result<Option<WebConfig>, String> {
        let settings = Settings::from_lua(text).map_err(|failure| failure.said)?;
        settings.web(PathBuf::from("/data"), password.map(str::to_owned))
    }

    #[test]
    fn off_unless_a_mode_says_otherwise() {
        assert!(web("", None).unwrap().is_none());
        assert!(
            web("daemon.web.mode = \"off\"\ndaemon.web.port = 1", None)
                .unwrap()
                .is_none()
        );
        assert!(web("daemon.web.mode = \"on\"", None).is_err());
    }

    #[test]
    fn tailscale_listens_on_a_unix_socket_only() {
        let config = web(
            "daemon.web.mode = \"tailscale\"\ndaemon.web.login = \"jack@github\"",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.socket, PathBuf::from("/data/web.sock"));
        assert_eq!(config.login.as_deref(), Some("jack@github"));
        assert!(config.password.is_none());
        assert!(config.ask, "a window may let a device in by default");
        assert_eq!(config.expire_after, DEFAULT_EXPIRY);
        let config = web(
            "daemon.web.mode = \"tailscale\"\ndaemon.web.socket = \"/run/w.sock\"",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.socket, PathBuf::from("/run/w.sock"));
        for listening in ["daemon.web.port = 9000", "daemon.web.address = \"0.0.0.0\""] {
            let err = web(
                &format!("daemon.web.mode = \"tailscale\"\n{listening}"),
                None,
            )
            .unwrap_err();
            assert!(err.contains("unix socket"), "{err}");
            assert!(err.starts_with("daemon/init.lua:2: "), "{err}");
        }
    }

    #[test]
    fn public_needs_a_certificate_a_password_and_its_origin() {
        let err = web("daemon.web.mode = \"public\"", None).unwrap_err();
        assert!(err.contains("certificate"), "{err}");
        let base = "daemon.web.mode = \"public\"\ndaemon.web.certificate = \"c.pem\"\n\
                    daemon.web.key = \"k.pem\"\n";
        let err = web(base, None).unwrap_err();
        assert!(err.contains("origin"), "{err}");
        let base = format!("{base}daemon.web.origin = \"https://Example.com:8443/\"\n");
        let err = web(&base, None).unwrap_err();
        assert!(err.contains("password"), "{err}");
        let err = web(&base, Some("hunter2")).unwrap_err();
        assert!(
            err.contains("password:"),
            "a plain password isn't a hash: {err}"
        );
        let hash = crate::web::hash_password("hunter2");
        let config = web(&base, Some(&hash)).unwrap().unwrap();
        assert!(!config.ask, "strangers don't ask windows by default");
        let config = web(
            &format!(
                "{base}daemon.web.address = \"::\"\ndaemon.web.port = 8443\n\
                 daemon.web.expire_days = 7\ndaemon.web.password_days = 30\ndaemon.web.ask = true"
            ),
            Some(&hash),
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.address, "[::]:8443".parse().unwrap());
        assert_eq!(config.origin.as_deref(), Some("https://example.com:8443"));
        assert_eq!(config.expire_after, Duration::from_hours(7 * 24));
        assert_eq!(config.password_every, Some(Duration::from_hours(30 * 24)));
        assert!(config.ask);
        let wrong_origin = base.replace("https://Example.com:8443/", "http://x");
        assert!(web(&wrong_origin, Some(&hash)).is_err());
        assert!(web(&format!("{base}daemon.web.socket = \"/s\""), Some(&hash)).is_err());
        assert!(web(&format!("{base}daemon.web.address = \"here\""), Some(&hash)).is_err());
    }

    #[test]
    fn lock_after_is_read_within_its_range() {
        let base = "daemon.web.mode = \"tailscale\"\n";
        assert_eq!(web(base, None).unwrap().unwrap().lock_after, LOCK_AFTER);
        let config = web(&format!("{base}daemon.web.lock_after = 5"), None)
            .unwrap()
            .unwrap();
        assert_eq!(config.lock_after, 5);
        for wrong in ["0", "10001", "\"5\""] {
            assert!(
                web(&format!("{base}daemon.web.lock_after = {wrong}"), None).is_err(),
                "{wrong}"
            );
        }
    }

    #[test]
    fn a_name_that_isnt_a_setting_is_refused_saying_which_there_are() {
        let err = web("daemon.web.lock_afer = 3", None).unwrap_err();
        assert!(
            err.contains("there's no setting daemon.web.lock_afer"),
            "{err}"
        );
        assert!(err.contains("lock_after"), "{err}");
    }
}
