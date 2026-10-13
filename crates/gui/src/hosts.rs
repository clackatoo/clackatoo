//! The machines a window can connect to with `--remote`, for the picker
//! `clackatoo --connect-picker` and `:connect` open (see
//! [`crate::connect`]), and for the launcher's actions, which
//! `clackatoo desktop` writes.
//!
//! They come from three places:
//!
//! - **Machines connected to recently,** newest first: a line each in
//!   `clackatoo/remotes` under `$XDG_STATE_HOME`, written by the window
//!   each time a connection to another machine is made.
//! - **`~/.ssh/config`,** and the files it includes: each `Host` that
//!   names one machine. Patterns, with `*`, `?` or `!`, name many, so
//!   they're left out, as is everything under `Match`, which applies only
//!   when its conditions hold.
//! - **Tailscale's peers** that are online, from `tailscale status
//!   --json`, if Tailscale is installed.
//!
//! Only names that look like a machine are taken, from any of them (see
//! [`is_host`]), so nothing odd ever reaches ssh's command line or a
//! desktop entry.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How many machines connected to recently are kept.
pub const RECENT_KEPT: usize = 20;

/// How many of the machines connected to recently the launcher's entry
/// offers, as actions of their own.
pub const LAUNCHER_HOSTS: usize = 3;

/// How deep `Include`s may nest, as ssh allows.
const INCLUDE_DEPTH: u32 = 16;

/// Whether `text` names a machine as `--remote` takes one:
/// `[user@]host[:port]`, where the host is a name or an IPv4 address of
/// letters, digits, `.`, `_` and `-`, or an IPv6 address in brackets, and
/// the port a number from 1 to 65535. Nothing in it is special to a
/// shell, ssh's options or a desktop entry.
#[must_use]
pub fn is_host(text: &str) -> bool {
    let word = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['-', '.'])
            && (part.chars()).all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    };
    if text.len() > 255 {
        return false;
    }
    let (user, rest) = match text.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, text),
    };
    if user.is_some_and(|user| !word(user)) {
        return false;
    }
    let port_ok =
        |port: &str| !port.starts_with('0') && port.parse::<u16>().is_ok_and(|port| port > 0);
    if let Some(bracketed) = rest.strip_prefix('[') {
        let Some((address, after)) = bracketed.split_once(']') else {
            return false;
        };
        let address_ok = address.contains(':')
            && (address.chars()).all(|ch| ch.is_ascii_hexdigit() || matches!(ch, ':' | '.'));
        return address_ok && (after.is_empty() || after.strip_prefix(':').is_some_and(port_ok));
    }
    match rest.split_once(':') {
        Some((host, port)) => word(host) && port_ok(port),
        None => word(rest),
    }
}

/// Where the machines connected to recently are kept.
#[must_use]
pub fn recent_path() -> Option<PathBuf> {
    crate::tutor::state_dir().map(|dir| dir.join("clackatoo/remotes"))
}

/// The machines connected to recently, newest first, or none if the file
/// isn't there or can't be read.
#[must_use]
pub fn recent() -> Vec<String> {
    recent_path().map_or_else(Vec::new, |path| read_recent(&path))
}

/// The machines in the file at `path`, newest first: a line each. Lines
/// that don't name one, as an edit by hand might leave, are skipped.
pub(crate) fn read_recent(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut hosts: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim) {
        if is_host(line) && !hosts.iter().any(|host| host == line) {
            hosts.push(line.to_owned());
        }
        if hosts.len() == RECENT_KEPT {
            break;
        }
    }
    hosts
}

/// `hosts`, newest first, with `host` just connected to: first, once, and
/// no more than [`RECENT_KEPT`] in all.
pub(crate) fn remember(hosts: &[String], host: &str) -> Vec<String> {
    let mut list = Vec::with_capacity(RECENT_KEPT);
    list.push(host.to_owned());
    list.extend((hosts.iter()).filter(|kept| *kept != host).cloned());
    list.truncate(RECENT_KEPT);
    list
}

/// Adds `host` to the machines connected to recently, in the file at
/// `path`. Returns them before and after, or `None` if nothing changed,
/// as when it was the newest already, so nothing was written. The file
/// is written whole, beside it and renamed over it, so a window reading
/// it meanwhile reads the old list or the new, never half of one.
pub(crate) fn record(
    path: &Path,
    host: &str,
) -> std::io::Result<Option<(Vec<String>, Vec<String>)>> {
    if !is_host(host) {
        return Ok(None);
    }
    let before = read_recent(path);
    let after = remember(&before, host);
    if after == before {
        return Ok(None);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = after.join("\n");
    text.push('\n');
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("remotes");
    let beside = path.with_file_name(format!(".{name}.{}", std::process::id()));
    std::fs::write(&beside, text)?;
    std::fs::rename(&beside, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&beside);
    })?;
    Ok(Some((before, after)))
}

/// Has `program`, clackatoo, rewrite the launcher's entry with the
/// machines connected to recently, if clackatoo wrote the entry: `program
/// desktop refresh-hosts`, which keeps the program the entry starts. Waits
/// for it, so it's for a thread of its own.
pub(crate) fn refresh_launcher(program: &Path) {
    let refreshed = Command::new(program)
        .args(["desktop", "refresh-hosts"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match refreshed {
        Ok(output) if !output.status.success() => log::warn!(
            "the launcher's machines couldn't be updated: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Ok(_) => {}
        Err(err) => log::warn!("couldn't run {}: {err}", program.display()),
    }
}

/// The machines `~/.ssh/config` names, in `home`, and the files it
/// includes, in the order they're named.
pub(crate) fn ssh_config_hosts(home: &Path) -> Vec<String> {
    let ssh = home.join(".ssh");
    let mut hosts = Vec::new();
    read_ssh_config(&ssh.join("config"), &ssh, home, 0, &mut hosts);
    hosts
}

/// Adds the machines the ssh config at `path` names to `hosts`, `depth`
/// includes in. Relative `Include`s are from `ssh`, `~/.ssh`.
fn read_ssh_config(path: &Path, ssh: &Path, home: &Path, depth: u32, hosts: &mut Vec<String>) {
    if depth > INCLUDE_DEPTH {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut in_match = false;
    for line in text.lines() {
        let Some((keyword, words)) = directive(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                in_match = false;
                for word in words {
                    let pattern = word.contains(['*', '?', '!']);
                    if !pattern && is_host(&word) && !hosts.contains(&word) {
                        hosts.push(word);
                    }
                }
            }
            "match" => in_match = true,
            // An Include under Match is read only when its conditions
            // hold, which can't be known here.
            "include" if !in_match => {
                for word in words {
                    for included in include_paths(&word, ssh, home) {
                        read_ssh_config(&included, ssh, home, depth + 1, hosts);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A line of an ssh config as its keyword, lowercased, and its words: the
/// two parted by spaces or an `=`, and words in double quotes kept whole.
/// `None` for a blank line or a comment.
fn directive(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|ch: char| ch.is_whitespace() || ch == '=')
        .unwrap_or(line.len());
    let keyword = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let mut words = Vec::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.next_if(|ch| ch.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else { break };
        if first == '#' {
            break;
        }
        let mut word = String::new();
        if first == '"' {
            chars.next();
            word.extend(chars.by_ref().take_while(|&ch| ch != '"'));
        } else {
            while let Some(ch) = chars.next_if(|ch| !ch.is_whitespace()) {
                word.push(ch);
            }
        }
        words.push(word);
    }
    Some((keyword, words))
}

/// The files an `Include` of `pattern` reads, in order: `~/` is the home
/// directory, a relative path is from `ssh`, and `*` and `?` match in any
/// part of the path, as glob(3) does.
fn include_paths(pattern: &str, ssh: &Path, home: &Path) -> Vec<PathBuf> {
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if pattern.starts_with('/') => PathBuf::from(pattern),
        None => ssh.join(pattern),
    };
    let mut found = vec![PathBuf::new()];
    for part in path.components() {
        let part = part.as_os_str();
        let Some(glob) = part.to_str().filter(|part| part.contains(['*', '?'])) else {
            for path in &mut found {
                path.push(part);
            }
            continue;
        };
        let mut matched = Vec::new();
        for dir in &found {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| glob_matches(glob, name))
                .collect();
            names.sort();
            matched.extend(names.into_iter().map(|name| dir.join(name)));
        }
        found = matched;
    }
    found.retain(|path| path.is_file());
    found
}

/// Whether `name` matches `glob`, where `*` is any run of characters and
/// `?` any one, except that neither matches a leading `.`, as in glob(3).
fn glob_matches(glob: &str, name: &str) -> bool {
    if name.starts_with('.') && !glob.starts_with('.') {
        return false;
    }
    let (glob, name): (Vec<char>, Vec<char>) = (glob.chars().collect(), name.chars().collect());
    // The latest `*`, and where in the name it was tried up to.
    let (mut g, mut n, mut star) = (0, 0, None);
    while n < name.len() {
        match glob.get(g) {
            Some('*') => {
                star = Some((g, n));
                g += 1;
            }
            Some(&ch) if ch == '?' || ch == name[n] => {
                g += 1;
                n += 1;
            }
            _ => match star {
                Some((at, tried)) => {
                    g = at + 1;
                    n = tried + 1;
                    star = Some((at, tried + 1));
                }
                None => return false,
            },
        }
    }
    glob[g..].iter().all(|&ch| ch == '*')
}

/// A Tailscale peer that's online.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Peer {
    /// What to connect to: its `MagicDNS` name, if `MagicDNS` is on, or else
    /// its Tailscale IPv4 address.
    pub(crate) host: String,
    /// Its operating system, as Tailscale says, such as `linux`.
    pub(crate) os: String,
    /// Its Tailscale IPv4 address.
    pub(crate) address: String,
}

/// The peers that are online in what `tailscale status --json` printed,
/// by name. Phones are left out: they don't run clackatoo.
pub(crate) fn tailscale_peers(json: &str) -> Vec<Peer> {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let magic_dns = status["CurrentTailnet"]["MagicDNSEnabled"]
        .as_bool()
        .unwrap_or(false);
    let Some(peers) = status["Peer"].as_object() else {
        return Vec::new();
    };
    let mut found: Vec<Peer> = (peers.values())
        .filter(|peer| peer["Online"].as_bool() == Some(true))
        .filter_map(|peer| {
            let os = peer["OS"].as_str().unwrap_or_default();
            if matches!(os, "iOS" | "android") {
                return None;
            }
            let address = (peer["TailscaleIPs"].as_array()?.iter())
                .filter_map(serde_json::Value::as_str)
                .find(|address| !address.contains(':'))?;
            // The first label of its DNS name is what MagicDNS answers to.
            let name = (peer["DNSName"].as_str().unwrap_or_default())
                .split('.')
                .next()
                .unwrap_or_default();
            let host = if magic_dns && is_host(name) {
                name
            } else {
                address
            };
            is_host(host).then(|| Peer {
                host: host.to_owned(),
                os: os.to_owned(),
                address: address.to_owned(),
            })
        })
        .collect();
    found.sort_by(|a, b| a.host.cmp(&b.host));
    found
}

/// What `tailscale status --json` prints, if Tailscale is installed and
/// answers within `timeout`. A Tailscale that doesn't is stopped.
pub(crate) fn tailscale_status(timeout: Duration) -> Option<String> {
    let mut tailscale = Command::new("tailscale");
    tailscale.args(["status", "--json"]);
    printed_within(tailscale, timeout)
}

/// What `command` prints, if it runs, prints it within `timeout`, and
/// succeeds. One that takes longer is stopped.
fn printed_within(mut command: Command, timeout: Duration) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sent, printed) = mpsc::channel();
    // Read as it prints, so a long status never fills the pipe and waits.
    let reader = std::thread::Builder::new()
        .name("tailscale".into())
        .spawn(move || {
            let mut text = String::new();
            let read = stdout.read_to_string(&mut text);
            let _ = sent.send(read.map(|_| text));
        });
    let text = match reader {
        Ok(_) => printed.recv_timeout(timeout).ok().and_then(Result::ok),
        Err(_) => None,
    };
    if text.is_none() {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    text.filter(|_| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_names_with_a_user_and_port_and_nothing_odd() {
        for good in [
            "desktop",
            "jack@desktop",
            "jack@desktop:2222",
            "desktop.example.org",
            "jack-ubuntu-desktop",
            "build_box",
            "100.64.0.7",
            "[fd7a:115c::1]",
            "me@[fd7a:115c::1]:22",
        ] {
            assert!(is_host(good), "{good}");
        }
        for bad in [
            "",
            "-oProxyCommand=evil",
            "jack@-desk",
            ".hidden",
            "desk top",
            "desk;rm",
            "desk$HOME",
            "desk%u",
            "desk\nx",
            "desk:0",
            "desk:65536",
            "desk:22:22",
            "desk:",
            "@desk",
            "fd7a::1",
            "[desk]",
            "[fd7a::1",
            "[fd7a::1]x",
            "\"desk\"",
            "dësk",
        ] {
            assert!(!is_host(bad), "{bad:?}");
        }
        assert!(!is_host(&"a".repeat(256)));
    }

    #[test]
    fn the_newest_comes_first_once_and_the_list_is_capped() {
        let hosts = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(remember(&[], "a"), hosts(&["a"]));
        assert_eq!(
            remember(&hosts(&["a", "b", "c"]), "c"),
            hosts(&["c", "a", "b"])
        );
        assert_eq!(remember(&hosts(&["a", "b"]), "a"), hosts(&["a", "b"]));
        let full: Vec<String> = (0..RECENT_KEPT).map(|n| format!("h{n}")).collect();
        let after = remember(&full, "new");
        assert_eq!(after.len(), RECENT_KEPT);
        assert_eq!(after[0], "new");
        assert_eq!(after[RECENT_KEPT - 1], format!("h{}", RECENT_KEPT - 2));
    }

    #[test]
    fn recording_writes_only_what_changed_and_reads_back_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/clackatoo/remotes");
        assert_eq!(read_recent(&path), Vec::<String>::new());

        let first = record(&path, "desktop").unwrap();
        assert_eq!(first, Some((vec![], vec!["desktop".to_owned()])));
        record(&path, "jack@server:2222").unwrap();
        assert_eq!(read_recent(&path), ["jack@server:2222", "desktop"]);
        // Again, and nothing changes, so nothing's written.
        assert_eq!(record(&path, "jack@server:2222").unwrap(), None);
        // Nor for something that names no machine.
        assert_eq!(record(&path, "-oEvil").unwrap(), None);
        assert_eq!(
            record(&path, "desktop").unwrap().map(|(_, after)| after),
            Some(vec!["desktop".to_owned(), "jack@server:2222".to_owned()])
        );
        // No file is left beside it.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["remotes"]);

        // Lines edited in by hand that name no machine, and repeats, are
        // skipped, and only so many are read.
        let mut text = String::from("a\n\nnot a host\na\n  b  \n");
        text.extend((0..RECENT_KEPT).map(|n| format!("h{n}\n")));
        std::fs::write(&path, text).unwrap();
        let read = read_recent(&path);
        assert_eq!(read[..3], ["a", "b", "h0"]);
        assert_eq!(read.len(), RECENT_KEPT);
    }

    #[test]
    fn ssh_config_hosts_follow_includes_and_skip_patterns_and_match_blocks() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(ssh.join("config.d")).unwrap();
        std::fs::create_dir_all(home.path().join("elsewhere")).unwrap();
        std::fs::write(
            ssh.join("config"),
            "# My machines\n\
             Host desktop laptop\n\
             \tHostName 192.168.1.10\n\
             host=server\n\
             HOST \"quoted\" # a comment\n\
             Host *.example.org !nope dev? *\n\
             Include config.d/*\n\
             Include ~/elsewhere/extra\n\
             Include /no/such/file\n\
             Match host desktop exec \"true\"\n\
             \tUser jack\n\
             \tInclude matched\n\
             Host after-match\n\
             Host desktop\n\
             Host -oProxyCommand=evil\n",
        )
        .unwrap();
        std::fs::write(ssh.join("config.d/10-work"), "Host work-box\n  Port 2222\n").unwrap();
        std::fs::write(
            ssh.join("config.d/20-home"),
            "Host nas\nInclude config.d/30-nested\n",
        )
        .unwrap();
        std::fs::write(ssh.join("config.d/30-nested"), "Host nested\n").unwrap();
        // Hidden files aren't matched by `*`, as with glob(3).
        std::fs::write(ssh.join("config.d/.swap"), "Host swap\n").unwrap();
        std::fs::write(home.path().join("elsewhere/extra"), "Host extra\n").unwrap();
        std::fs::write(ssh.join("matched"), "Host matched\n").unwrap();

        assert_eq!(
            ssh_config_hosts(home.path()),
            [
                "desktop",
                "laptop",
                "server",
                "quoted",
                "work-box",
                "nas",
                "nested",
                "extra",
                "after-match"
            ]
        );
    }

    #[test]
    fn includes_that_include_themselves_stop() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("config"), "Host loop\nInclude config\n").unwrap();
        assert_eq!(ssh_config_hosts(home.path()), ["loop"]);
        // No config at all is no machines.
        assert!(ssh_config_hosts(&home.path().join("nobody")).is_empty());
    }

    #[test]
    fn globs_match_as_glob_does() {
        assert!(glob_matches("*", "a"));
        assert!(glob_matches("*.conf", "x.conf"));
        assert!(!glob_matches("*.conf", "x.conf.bak"));
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "ac"));
        assert!(glob_matches("a*b*c", "aXbYbZc"));
        assert!(!glob_matches("*", ".hidden"));
        assert!(glob_matches(".*", ".hidden"));
        assert!(glob_matches("exact", "exact"));
        assert!(!glob_matches("exact", "exactly"));
    }

    /// What `tailscale status --json` prints, cut down, with made-up names
    /// and addresses.
    const TAILSCALE: &str = include_str!("../tests/fixtures/tailscale-status.json");

    #[test]
    fn tailscale_peers_are_those_online_by_magic_dns_name() {
        assert_eq!(
            tailscale_peers(TAILSCALE),
            [
                Peer {
                    host: "build-server".into(),
                    os: "linux".into(),
                    address: "100.101.102.2".into(),
                },
                Peer {
                    host: "studio-mac".into(),
                    os: "macOS".into(),
                    address: "100.101.102.3".into(),
                },
            ]
        );
    }

    #[test]
    fn a_program_is_heard_only_if_it_answers_in_time_and_succeeds() {
        let sh = |script: &str| {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        };
        let timeout = Duration::from_secs(5);
        assert_eq!(
            printed_within(sh("printf '{}'"), timeout).as_deref(),
            Some("{}")
        );
        assert_eq!(printed_within(sh("printf x; exit 1"), timeout), None);
        assert_eq!(
            printed_within(Command::new("/no/such/program"), timeout),
            None
        );
        // One that hangs is stopped.
        let started = std::time::Instant::now();
        assert_eq!(
            printed_within(sh("exec sleep 30"), Duration::from_millis(100)),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // A long answer doesn't fill the pipe and wait.
        let long = printed_within(sh("head -c 1000000 /dev/zero | tr '\\0' x"), timeout);
        assert_eq!(long.map(|text| text.len()), Some(1_000_000));
    }

    #[test]
    #[ignore = "asks the Tailscale installed here, if any"]
    fn the_tailscale_here_answers_in_time() {
        let started = std::time::Instant::now();
        let status = tailscale_status(Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(3));
        if let Some(status) = status {
            eprintln!("{} peers online", tailscale_peers(&status).len());
        }
    }

    #[test]
    fn tailscale_peers_go_by_address_without_magic_dns_and_nothing_is_no_peers() {
        let without = TAILSCALE.replace("\"MagicDNSEnabled\": true", "\"MagicDNSEnabled\": false");
        let hosts: Vec<String> = (tailscale_peers(&without).into_iter())
            .map(|peer| peer.host)
            .collect();
        assert_eq!(hosts, ["100.101.102.2", "100.101.102.3"]);
        assert!(tailscale_peers("").is_empty());
        assert!(tailscale_peers("{\"BackendState\": \"Stopped\"}").is_empty());
        assert!(tailscale_peers("not json").is_empty());
    }
}
