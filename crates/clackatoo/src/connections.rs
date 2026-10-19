//! `clackatoo connect`, `disconnect`, `connections` and `machine`: the
//! daemon's connections to the daemons on other machines, and which
//! machine it is. Hidden for now: a way to drive and try the connections
//! until windows have `:connect`. `connect --no-share` keeps a machine
//! from being passed on to the daemons connected to this one, and
//! `connections` says which go through other machines, and which aren't
//! passed on.
//!
//! `connect` asks the daemon to connect, answers the questions ssh asks as
//! it logs in (a password, a passphrase, whether to trust a new machine),
//! on this terminal, or from standard input if it isn't one, and waits to
//! say how the first try went. The daemon keeps the connection from then
//! on, trying again whenever it's lost, until `disconnect`.

use std::io::{self, BufRead as _, Write as _};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Connection as Link, ConnectionState, Event, Request, Response, wire};
use clap::Args;

use crate::Connection;

/// The longest `connect` waits to hear how the first try went: past ssh's
/// two minutes to log in.
const CONNECT_WAIT: Duration = Duration::from_secs(150);

#[derive(Debug, Args)]
pub(crate) struct ConnectArgs {
    /// The machine: `host`, `user@host`, `host:port`, or a name from your
    /// ssh config.
    host: String,
    /// The shell command that runs clackatoo on that machine, if it isn't
    /// `clackatoo` on the PATH there.
    #[arg(long, value_name = "COMMAND")]
    command: Option<String>,
    /// Don't pass the machine on to the daemons connected to this one, as
    /// for a server others share.
    #[arg(long, conflicts_with = "share")]
    no_share: bool,
    /// Pass it on again, after `--no-share`.
    #[arg(long)]
    share: bool,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
pub(crate) struct DisconnectArgs {
    /// The machine, as it was connected to.
    host: String,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
pub(crate) struct LentArgs {
    /// `reclaim N` gives back the workspace numbered N, as listed, making
    /// it one of this machine's own, shown in every window, named after
    /// the machine it was lent to.
    #[arg(value_names = ["reclaim", "N"], num_args = 0..=2)]
    reclaim: Vec<String>,
    #[command(flatten)]
    connection: Connection,
}

/// Lists the workspaces the daemon lent the daemons on other machines, or
/// gives one back, as `args` says.
pub(crate) fn lent(args: &LentArgs) -> anyhow::Result<()> {
    let mut client = crate::connect(&args.connection)?;
    anyhow::ensure!(
        client.daemon_has(wire::LEND),
        "the daemon is from an older build, which lends no panes: `clackatoo upgrade` moves it \
         onto this one"
    );
    let request = match &args.reclaim[..] {
        [] => Request::Lent,
        [word, number] if word == "reclaim" => Request::Unlend {
            workspace: clackatoo_protocol::WorkspaceId(
                number
                    .parse()
                    .map_err(|_| anyhow::anyhow!("{number} isn't a workspace's number"))?,
            ),
            close_idle: false,
        },
        _ => anyhow::bail!("lent takes nothing, or reclaim N"),
    };
    match client.request(&request)? {
        Response::Text(said) => println!("{}", said.trim_end()),
        Response::Error(err) => anyhow::bail!("{err}"),
        other => anyhow::bail!("the daemon answered with {other:?}"),
    }
    Ok(())
}

/// A client of the daemon `connection` says, which must connect to other
/// machines.
fn federating(connection: &Connection) -> anyhow::Result<clackatoo_daemon::Client> {
    let client = crate::connect(connection)?;
    anyhow::ensure!(
        client.daemon_has(wire::FEDERATION),
        "the daemon is from an older build, which doesn't connect to other machines: \
         `clackatoo upgrade` moves it onto this one"
    );
    Ok(client)
}

/// Asks the daemon to connect to `args.host`, answers ssh's questions,
/// and says how the first try went.
pub(crate) fn connect(args: &ConnectArgs) -> anyhow::Result<()> {
    let mut client = federating(&args.connection)?;
    let share = match (args.share, args.no_share) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    anyhow::ensure!(
        share.is_none() || client.daemon_has(wire::CHAIN),
        "the daemon is from an older build, which passes no machine on: `clackatoo upgrade` \
         moves it onto this one, which can be asked not to"
    );
    client.watch_connections(true)?;
    client.connect_sharing(&args.host, args.command.as_deref(), share)?;
    // As it is now: anything heard before is older.
    let find = |list: Vec<Link>| list.into_iter().find(|link| link.host == args.host);
    let mut link = find(client.watch_connections(true)?);
    let deadline = Instant::now() + CONNECT_WAIT;
    loop {
        if let Some(link) = &link {
            match &link.state {
                ConnectionState::Connected { wire } => {
                    let machine = (link.machine.as_ref())
                        .map(|machine| format!(": {} ({})", machine.name, machine.id))
                        .unwrap_or_default();
                    println!("connected to {}{machine}, at protocol {wire}", link.host);
                    return Ok(());
                }
                ConnectionState::Down { reason } => anyhow::bail!(
                    "couldn't connect to {}: {reason}\nThe daemon tries again by itself; \
                     `clackatoo disconnect {}` stops it.",
                    link.host,
                    link.host
                ),
                ConnectionState::Stopped { reason } => {
                    anyhow::bail!("not connected to {}: {reason}", link.host)
                }
                ConnectionState::NeedsPassword => anyhow::bail!(
                    "the connection to {} needs a question answered, and waits for a window to \
                     ask it",
                    link.host
                ),
                ConnectionState::Connecting | ConnectionState::Asking | ConnectionState::Other => {}
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(
            !left.is_zero(),
            "the daemon is still connecting to {}",
            args.host
        );
        client.set_read_timeout(Some(left))?;
        match client.next_event() {
            Ok(Event::Connections(list)) => link = find(list),
            Ok(Event::Login {
                ask,
                host,
                question: Some(question),
            }) => {
                let answer = answer(&host, &question)?;
                client.answer_login(ask, answer)?;
            }
            Ok(_) => {}
            Err(clackatoo_daemon::ClientError::Io(err))
                if err.kind() == io::ErrorKind::WouldBlock =>
            {
                anyhow::bail!("the daemon is still connecting to {}", args.host);
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// The answer to `question`, which ssh asks logging in to `host`: typed
/// on this terminal, without being shown unless it's a yes or no, or else
/// the next line of standard input. `None`, turning it down, at the input's
/// end.
fn answer(host: &str, question: &str) -> anyhow::Result<Option<String>> {
    let stdin = io::stdin();
    if !rustix::termios::isatty(&stdin) {
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(None);
        }
        return Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()));
    }
    eprintln!("Logging in to {host}:");
    if question.to_lowercase().contains("yes/no") {
        eprint!("{question}");
        io::stderr().flush()?;
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()));
    }
    crate::web::prompt(question).map(Some)
}

/// Ends the daemon's connection to `args.host`.
pub(crate) fn disconnect(args: &DisconnectArgs) -> anyhow::Result<()> {
    federating(&args.connection)?.disconnect(&args.host)?;
    Ok(())
}

/// Forgets the identity key the daemon knows the machine of its
/// connection to `args.host` by.
pub(crate) fn forget(args: &DisconnectArgs) -> anyhow::Result<()> {
    let mut client = federating(&args.connection)?;
    anyhow::ensure!(
        client.daemon_has(wire::CHAIN),
        "the daemon is from an older build, which keeps no keys: `clackatoo upgrade` moves it \
         onto this one"
    );
    let forget = Request::Forget {
        host: args.host.clone(),
    };
    match client.request(&forget)? {
        Response::Text(said) => println!("{said}"),
        Response::Error(err) => anyhow::bail!("{err}"),
        other => anyhow::bail!("the daemon answered with {other:?}"),
    }
    Ok(())
}

/// Lists the daemon's connections, and how each is going.
pub(crate) fn list(connection: &Connection) -> anyhow::Result<()> {
    let links = federating(connection)?.watch_connections(false)?;
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{:<24}  {:<24}  {:<32}  STATE",
        "HOST", "MACHINE", "ID"
    )?;
    for link in links {
        let (name, id) = link.machine.map_or_else(
            || ("-".to_owned(), "-".to_owned()),
            |machine| (machine.name, machine.id.to_string()),
        );
        let mut state = state(&link.state);
        if !link.through.is_empty() {
            state = format!("{state}; through {}", link.through.join(" and "));
        }
        if link.unshared {
            state = format!("{state}; not passed on");
        }
        writeln!(out, "{:<24}  {name:<24}  {id:<32}  {state}", link.host)?;
    }
    Ok(())
}

/// Says which machine the daemon is: its name and ID.
pub(crate) fn machine(connection: &Connection) -> anyhow::Result<()> {
    let workspaces = federating(connection)?.watch_workspaces()?;
    let machine = workspaces
        .machine
        .ok_or_else(|| anyhow::anyhow!("the daemon didn't say which machine it is"))?;
    println!("{} {}", machine.name, machine.id);
    Ok(())
}

/// `state`, in words.
fn state(state: &ConnectionState) -> String {
    match state {
        ConnectionState::Connecting => "connecting".to_owned(),
        ConnectionState::Asking => "asking a window ssh's question".to_owned(),
        ConnectionState::NeedsPassword => "needs a password: waiting for a window".to_owned(),
        ConnectionState::Connected { wire } => format!("connected, at protocol {wire}"),
        ConnectionState::Down { reason } => format!("down, trying again: {reason}"),
        ConnectionState::Stopped { reason } => format!("stopped: {reason}"),
        ConnectionState::Other => "in a state this build doesn't know".to_owned(),
    }
}
