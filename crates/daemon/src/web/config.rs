//! The web app's settings: `daemon/web.toml` in clackatoo's config
//! directory, read when the daemon starts.
//!
//! ```toml
//! # Off unless this says otherwise.
//! mode = "tailscale"        # or "public", or "off"
//! # tailscale: the unix socket `tailscale serve` connects to, which only
//! # you and root can reach; by default, web.sock in the web app's data.
//! socket = "/home/you/.local/share/clackatoo/web/web.sock"
//! # tailscale: the Tailscale login allowed in; else the first device's,
//! # which only a window can let in.
//! login = "you@github"
//! # Written by `clackatoo web password`: an Argon2id hash, never the
//! # password itself. Optional with tailscale, required with public.
//! password = "$argon2id$v=19$m=19456,t=2,p=1$…"
//! # public: where to listen, and the certificate to show, read again
//! # when its files change.
//! address = "0.0.0.0"
//! port = 8765
//! certificate = "/etc/letsencrypt/live/example.com/fullchain.pem"
//! key = "/etc/letsencrypt/live/example.com/privkey.pem"
//! # The address the app is reached at: requests for any other name, or
//! # from any other page, are refused. Required with public.
//! origin = "https://example.com:8765"
//! # A device unused this long is unpaired.
//! expire_days = 90
//! # Ask for the password again after this many days; 0 never does.
//! password_days = 0
//! # Whether a device may ask to be allowed from a window: by default
//! # with tailscale, not with public.
//! ask = true
//! ```

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
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
        }
    }

    /// Whether it's reached through Tailscale.
    pub(crate) fn tailscale(&self) -> bool {
        self.mode == WebMode::Tailscale
    }
}

/// The settings in `web.toml` at `path`, keeping what they keep in
/// `data`: `None` if it isn't there, or says `off`, or doesn't make
/// sense, which is logged.
#[must_use]
pub fn read(path: &Path, data: PathBuf) -> Option<WebConfig> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("{} couldn't be read: {err}", path.display());
            return None;
        }
    };
    parse(&text, data)
        .map_err(|err| log::error!("{}: {err}; the web app is off", path.display()))
        .ok()
        .flatten()
}

/// The settings `text` says, keeping what they keep in `data`: `None` if
/// it's off.
///
/// # Errors
///
/// If `text` isn't TOML, or a setting doesn't make sense.
pub fn parse(text: &str, data: PathBuf) -> Result<Option<WebConfig>, String> {
    let document: toml_edit::DocumentMut = text.parse().map_err(|err| format!("{err}"))?;
    let text = |key: &str| -> Result<Option<String>, String> {
        document.get(key).map_or(Ok(None), |item| {
            (item.as_str().map(|text| Some(text.to_owned())))
                .ok_or_else(|| format!("{key} is a string"))
        })
    };
    let number = |key: &str| -> Result<Option<u64>, String> {
        document.get(key).map_or(Ok(None), |item| {
            (item.as_integer().and_then(|n| u64::try_from(n).ok()))
                .map(Some)
                .ok_or_else(|| format!("{key} is a number, 0 or more"))
        })
    };
    let mode = match text("mode")?.as_deref() {
        None | Some("off") => return Ok(None),
        Some("tailscale") => WebMode::Tailscale,
        Some("public") => WebMode::Public {
            certificate: text("certificate")?
                .ok_or("public needs a certificate: the path of its PEM file")?
                .into(),
            key: text("key")?
                .ok_or("public needs a key: the path of the certificate's private key")?
                .into(),
        },
        Some(other) => return Err(format!("mode is off, tailscale or public, not {other:?}")),
    };
    let mut config = WebConfig::new(mode, data);
    if config.tailscale() {
        if document.contains_key("port") || document.contains_key("address") {
            return Err(
                "port and address are for public: tailscale listens on a unix socket, \
                 which `tailscale serve --bg unix:PATH` serves"
                    .into(),
            );
        }
        if let Some(socket) = text("socket")? {
            config.socket = PathBuf::from(socket);
        }
    } else {
        if document.contains_key("socket") {
            return Err("socket is for tailscale: public listens on address and port".into());
        }
        if let Some(port) = number("port")? {
            let port = u16::try_from(port).map_err(|_| "port is at most 65535".to_owned())?;
            config.address.set_port(port);
        }
        if let Some(address) = text("address")? {
            let ip: IpAddr = (address.parse())
                .map_err(|_| format!("address is an IP address to listen on, not {address:?}"))?;
            config.address.set_ip(ip);
        }
    }
    config.login = text("login")?.filter(|login| !login.is_empty());
    config.password = text("password")?.filter(|hash| !hash.is_empty());
    if let Some(hash) = &config.password {
        super::auth::check_hash(hash).map_err(|err| format!("password: {err}"))?;
    } else if !config.tailscale() {
        return Err("public needs a password: `clackatoo web password` sets one".into());
    }
    if let Some(origin) = text("origin")? {
        let origin = origin.trim_end_matches('/').to_ascii_lowercase();
        if !origin.starts_with("https://") || origin[8..].contains('/') {
            return Err(format!(
                "origin is the address the app is reached at, as https://host:port, not {origin:?}"
            ));
        }
        config.origin = Some(origin);
    } else if !config.tailscale() {
        return Err(
            "public needs an origin: the address the app is reached at, as https://host:port"
                .into(),
        );
    }
    if let Some(days) = number("expire_days")? {
        if days == 0 {
            return Err("expire_days is at least 1".into());
        }
        config.expire_after = Duration::from_hours(days.saturating_mul(24));
    }
    config.password_every = number("password_days")?
        .filter(|&days| days > 0)
        .map(|days| Duration::from_hours(days.saturating_mul(24)));
    if let Some(ask) = document.get("ask") {
        config.ask = ask.as_bool().ok_or("ask is true or false")?;
    }
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Option<WebConfig>, String> {
        super::parse(text, PathBuf::from("/data"))
    }

    #[test]
    fn off_unless_a_mode_says_otherwise() {
        assert!(parse("").unwrap().is_none());
        assert!(parse("mode = \"off\"\nport = 1").unwrap().is_none());
        assert!(parse("mode = \"on\"").is_err());
    }

    #[test]
    fn tailscale_listens_on_a_unix_socket_only() {
        let config = parse("mode = \"tailscale\"\nlogin = \"jack@github\"")
            .unwrap()
            .unwrap();
        assert_eq!(config.socket, PathBuf::from("/data/web.sock"));
        assert_eq!(config.login.as_deref(), Some("jack@github"));
        assert!(config.password.is_none());
        assert!(config.ask, "a window may let a device in by default");
        assert_eq!(config.expire_after, DEFAULT_EXPIRY);
        let config = parse("mode = \"tailscale\"\nsocket = \"/run/w.sock\"")
            .unwrap()
            .unwrap();
        assert_eq!(config.socket, PathBuf::from("/run/w.sock"));
        for listening in ["port = 9000", "address = \"0.0.0.0\""] {
            let err = parse(&format!("mode = \"tailscale\"\n{listening}")).unwrap_err();
            assert!(err.contains("unix socket"), "{err}");
        }
    }

    #[test]
    fn public_needs_a_certificate_a_password_and_its_origin() {
        let err = parse("mode = \"public\"").unwrap_err();
        assert!(err.contains("certificate"), "{err}");
        let base = "mode = \"public\"\ncertificate = \"c.pem\"\nkey = \"k.pem\"\n";
        let err = parse(base).unwrap_err();
        assert!(err.contains("password"), "{err}");
        let err = parse(&format!("{base}password = \"hunter2\"")).unwrap_err();
        assert!(
            err.contains("password:"),
            "a plain password isn't a hash: {err}"
        );
        let hash = crate::web::hash_password("hunter2");
        let err = parse(&format!("{base}password = \"{hash}\"")).unwrap_err();
        assert!(err.contains("origin"), "{err}");
        let base = format!("{base}password = \"{hash}\"\norigin = \"https://Example.com:8443/\"\n");
        let config = parse(&base).unwrap().unwrap();
        assert!(!config.ask, "strangers don't ask windows by default");
        let config = parse(&format!(
            "{base}address = \"::\"\nport = 8443\nexpire_days = 7\npassword_days = 30\nask = true"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(config.address, "[::]:8443".parse().unwrap());
        assert_eq!(config.origin.as_deref(), Some("https://example.com:8443"));
        assert_eq!(config.expire_after, Duration::from_hours(7 * 24));
        assert_eq!(config.password_every, Some(Duration::from_hours(30 * 24)));
        assert!(config.ask);
        assert!(parse(&base.replace("https://Example.com:8443/", "http://x")).is_err());
        assert!(parse(&format!("{base}socket = \"/s\"")).is_err());
    }
}
