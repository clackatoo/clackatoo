//! The TCP ports the programs of a terminal listen on, as Linux's `/proc`
//! says, for the list of background tabs to show (CLACKA-121).
//!
//! A terminal's programs are the processes of its shell's session: the
//! shell leads one of its own, and what it starts stays in it unless it
//! sets out to leave, as a daemon does. A port is theirs if one of them
//! holds the socket listening on it: each socket in `/proc/net/tcp` and
//! `tcp6` in the `LISTEN` state has an inode, and a process holding it has
//! a descriptor in `/proc/PID/fd` linking to `socket:[INODE]`.
//!
//! Finding them reads a file for every process on the machine, and the
//! descriptors of those in the sessions asked about: a few milliseconds,
//! so it's done on a thread of its own ([`Finder`]), only when a window
//! asks, and never on the daemon's loop.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use mio::Waker;

/// The most processes looked at, so a machine with a great many can't
/// hold the thread for long.
const MOST_PROCESSES: usize = 100_000;

/// The most descriptors looked at of each process.
const MOST_DESCRIPTORS: usize = 10_000;

/// The most of each of `/proc/net/tcp` and `tcp6` read, in bytes.
const MOST_TABLE: usize = 16 * 1024 * 1024;

/// The longest name of a program kept, in characters: as long as Linux
/// keeps them.
const LONGEST_PROGRAM: usize = 15;

/// What's asked: for each key, such as a background tab's number, the
/// session whose processes' ports are wanted, by its leader's pid.
pub(crate) type Asked = Vec<(u32, u32)>;

/// What's found: for each key asked about, its ports, lowest first, and
/// the program listening on each.
pub(crate) type Found = Vec<(u32, Vec<(u16, String)>)>;

/// Finds ports on a thread of its own, started with the first ask, which
/// wakes the daemon's loop as each answer comes.
#[derive(Debug)]
pub(crate) struct Finder {
    asks: Sender<Asked>,
    found: Receiver<Found>,
}

impl Finder {
    /// A finder looking in `proc`, Linux's `/proc`, waking `waker` as
    /// each answer comes. `None` if its thread can't be started.
    pub(crate) fn start(proc: PathBuf, waker: Arc<Waker>) -> Option<Self> {
        let (asks, asked) = mpsc::channel::<Asked>();
        let (tell, found) = mpsc::channel();
        let started = std::thread::Builder::new()
            .name("ports".into())
            .spawn(move || {
                while let Ok(mut ask) = asked.recv() {
                    // Asks that came meanwhile are answered as one: the
                    // latest says what's wanted now.
                    while let Ok(later) = asked.try_recv() {
                        ask = later;
                    }
                    if tell.send(listening(&proc, &ask)).is_err() {
                        return;
                    }
                    if let Err(err) = waker.wake() {
                        log::warn!("waking the event loop for the ports found failed: {err}");
                    }
                }
            });
        match started {
            Ok(_) => Some(Self { asks, found }),
            Err(err) => {
                log::warn!("the ports background tabs listen on can't be found: {err}");
                None
            }
        }
    }

    /// Asks for the ports of `ask`'s sessions. Returns whether its thread
    /// took the ask: not if it has gone.
    pub(crate) fn ask(&self, ask: Asked) -> bool {
        self.asks.send(ask).is_ok()
    }

    /// What's been found since last taken, if anything has: the latest.
    pub(crate) fn take(&self) -> Option<Found> {
        let mut latest = None;
        loop {
            match self.found.try_recv() {
                Ok(found) => latest = Some(found),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return latest,
            }
        }
    }
}

/// The ports the processes of each of `asked`'s sessions listen on, as
/// `proc` says: every key asked about, those listening on none with none.
pub(crate) fn listening(proc: &Path, asked: &Asked) -> Found {
    let sockets = listening_sockets(proc);
    let sessions: HashSet<u32> = asked.iter().map(|&(_, session)| session).collect();
    let mut by_session: HashMap<u32, BTreeMap<u16, String>> = HashMap::new();
    if !sockets.is_empty() && !sessions.is_empty() {
        let processes = (std::fs::read_dir(proc).into_iter().flatten().flatten())
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .take(MOST_PROCESSES);
        for pid in processes {
            let Some((program, session)) = session_of(proc, pid) else {
                continue;
            };
            if !sessions.contains(&session) {
                continue;
            }
            for port in ports_held(proc, pid, &sockets) {
                (by_session.entry(session).or_default())
                    .entry(port)
                    .or_insert_with(|| program.clone());
            }
        }
    }
    (asked.iter())
        .map(|&(key, session)| {
            let ports = (by_session.get(&session).into_iter().flatten())
                .map(|(&port, program)| (port, program.clone()))
                .collect();
            (key, ports)
        })
        .collect()
}

/// The sockets listening for TCP connections, by inode, and the port each
/// listens on, as `proc`'s `net/tcp` and `net/tcp6` say.
fn listening_sockets(proc: &Path) -> HashMap<u64, u16> {
    let mut sockets = HashMap::new();
    for table in ["net/tcp", "net/tcp6"] {
        let Ok(file) = std::fs::File::open(proc.join(table)) else {
            continue;
        };
        read_table(std::io::BufReader::new(file), &mut sockets);
    }
    sockets
}

/// Notes the sockets listening that the lines of a table of `/proc/net`
/// say, read from `table`, in `sockets`: no more than [`MOST_TABLE`] bytes
/// of it, so a machine with a great many connections can't hold the
/// thread for long.
fn read_table(table: impl BufRead, sockets: &mut HashMap<u64, u16>) {
    let mut read = 0;
    for line in table.split(b'\n').skip(1) {
        let Ok(line) = line else { break };
        read += line.len() + 1;
        if read > MOST_TABLE {
            break;
        }
        if let Some((inode, port)) = std::str::from_utf8(&line).ok().and_then(listening_line) {
            sockets.insert(inode, port);
        }
    }
}

/// The inode and port of the socket a line of `/proc/net/tcp` is about,
/// if it's listening: `sl local_address rem_address st … inode`, the
/// address as hex `IP:PORT`, and the state `0A` for `LISTEN`.
fn listening_line(line: &str) -> Option<(u64, u16)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let (local, state, inode) = (fields.get(1)?, fields.get(3)?, fields.get(9)?);
    if *state != "0A" {
        return None;
    }
    let (_, port) = local.rsplit_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let inode = inode.parse().ok().filter(|&inode| inode != 0)?;
    Some((inode, port))
}

/// The name of process `pid`'s program, and the session it's in, as
/// `proc/PID/stat` says: `pid (comm) state ppid pgrp session …`, where
/// the name may hold anything, spaces and brackets included, so it's read
/// up to the last `)`.
fn session_of(proc: &Path, pid: u32) -> Option<(String, u32)> {
    let stat = std::fs::read(proc.join(pid.to_string()).join("stat")).ok()?;
    let stat = String::from_utf8_lossy(&stat);
    let (head, rest) = stat.rsplit_once(')')?;
    let (_, program) = head.split_once('(')?;
    let session = rest.split_whitespace().nth(3)?.parse().ok()?;
    let program = (program.chars())
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .take(LONGEST_PROGRAM)
        .collect();
    Some((program, session))
}

/// The ports of `sockets` process `pid` holds a socket listening on.
fn ports_held(proc: &Path, pid: u32, sockets: &HashMap<u64, u16>) -> BTreeSet<u16> {
    let fds = proc.join(pid.to_string()).join("fd");
    let mut ports = BTreeSet::new();
    let entries = std::fs::read_dir(&fds).into_iter().flatten().flatten();
    for entry in entries.take(MOST_DESCRIPTORS) {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let inode = (target.to_str())
            .and_then(|target| target.strip_prefix("socket:["))
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|inode| inode.parse::<u64>().ok());
        if let Some(port) = inode.and_then(|inode| sockets.get(&inode)) {
            ports.insert(*port);
        }
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_of_the_tcp_tables_are_read_for_the_sockets_listening() {
        let listening = "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 \
                         00000000  1000        0 123456 1 0000000000000000 100 0 0 10 0";
        assert_eq!(listening_line(listening), Some((123_456, 8080)));
        let connected = "   1: 0100007F:A1B2 0100007F:1F90 01 00000000:00000000 00:00000000 \
                         00000000  1000        0 654321 1 0000000000000000 20 4 30 10 -1";
        assert_eq!(listening_line(connected), None);
        let six = "   0: 00000000000000000000000001000000:0BB8 \
                   00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 \
                   00000000  1000        0 42 1 0000000000000000 100 0 0 10 0";
        assert_eq!(listening_line(six), Some((42, 3000)));
        assert_eq!(listening_line("garbage"), None);
    }

    #[test]
    fn a_table_is_read_no_further_than_the_most() {
        let row = |port: u16, inode: u64| {
            format!(
                "   0: 00000000:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 \
                 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0\n"
            )
        };
        let mut table = String::from("  sl  local_address ...\n");
        table.push_str(&row(8080, 1));
        // Past the most, nothing more is read.
        table.push_str(&" ".repeat(MOST_TABLE));
        table.push('\n');
        table.push_str(&row(9090, 2));
        let mut sockets = HashMap::new();
        read_table(table.as_bytes(), &mut sockets);
        assert_eq!(sockets, HashMap::from([(1, 8080)]));
    }

    #[test]
    fn a_programs_name_may_hold_spaces_and_brackets() {
        let proc = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proc.path().join("7")).unwrap();
        std::fs::write(
            proc.path().join("7/stat"),
            "7 (my (odd) prog) S 1 7 42 34816 7 4194560 0 0 0 0",
        )
        .unwrap();
        assert_eq!(
            session_of(proc.path(), 7),
            Some(("my (odd) prog".to_owned(), 42))
        );
        assert_eq!(session_of(proc.path(), 8), None);
    }

    #[test]
    fn a_port_this_process_listens_on_is_found_in_its_session() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let proc = Path::new("/proc");
        let (_, session) = session_of(proc, std::process::id()).unwrap();
        let found = listening(proc, &vec![(5, session), (6, u32::MAX)]);
        assert_eq!(found.len(), 2);
        let (key, ports) = &found[0];
        assert_eq!(*key, 5);
        assert!(ports.iter().any(|(found, _)| *found == port), "{ports:?}");
        assert_eq!(found[1], (6, Vec::new()), "a session with no processes");
        drop(listener);
    }
}
