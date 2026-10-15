//! `:PortForward`, as the window carries it out: forwarding ports from
//! this machine to the daemon's, when that's another one (see
//! [`clackatoo_daemon::forward`]). The window keeps the forwards, as they
//! outlive each connection to the daemon: they're handed to each new one
//! as it's made, and keep listening between.
//!
//! What each does is said in the bar, as is what a forward runs into,
//! once.

use std::fmt::Write as _;

use clackatoo_daemon::forward::{AddError, Forwards, Report, Spec};

use crate::command::Forwarding;

/// Does what `forwarding` says to `forwards`, the daemon being on the
/// machine called `machine`, or this one if `None`. Returns what to say,
/// if anything.
pub(crate) fn run(
    forwards: &mut Forwards,
    forwarding: Forwarding,
    machine: Option<&str>,
) -> Option<String> {
    Some(match forwarding {
        Forwarding::List => listing(&forwards.list(), machine),
        // The config runs in every window.
        Forwarding::Config(_) if machine.is_none() => return None,
        Forwarding::Add(spec) | Forwarding::Config(spec) => {
            let Some(machine) = machine else {
                return Some(
                    "This window's daemon is on this machine, so its ports are this \
                     machine's: there's nothing to forward"
                        .to_owned(),
                );
            };
            let asked = spec.clone();
            match forwards.add(spec) {
                Ok(spec) => format!(
                    "Forwarding port {} here to {}:{} on {machine}",
                    spec.local,
                    host(&spec),
                    spec.port
                ),
                Err(err) => refusal(&err, &asked),
            }
        }
        Forwarding::Stop(Some(local)) => match forwards.remove(local) {
            Some(spec) => format!("Stopped forwarding {spec}"),
            None => format!("Port {local} isn't forwarded"),
        },
        Forwarding::Stop(None) => match forwards.clear() {
            0 => "No ports are forwarded".to_owned(),
            1 => "Stopped forwarding the port".to_owned(),
            count => format!("Stopped forwarding {count} ports"),
        },
    })
}

/// What `report`, from a forward to the machine called `machine`, says in
/// the bar.
pub(crate) fn report(report: &Report, machine: &str) -> String {
    let mut said = report.describe(machine);
    if let Some(first) = said.get(..1) {
        said.replace_range(..1, &first.to_uppercase());
    }
    said
}

/// The forwards, and how many connections each has open, for the bar.
fn listing(list: &[(Spec, usize)], machine: Option<&str>) -> String {
    if list.is_empty() {
        return match machine {
            Some(_) => "No ports are forwarded: :PortForward 8080 forwards one".to_owned(),
            None => "No ports are forwarded: this window's daemon is on this machine".to_owned(),
        };
    }
    let mut said = String::from("Forwarding");
    for (at, (spec, open)) in list.iter().enumerate() {
        said.push_str(if at == 0 { " " } else { " · " });
        let _ = write!(said, "{spec}");
        match open {
            0 => {}
            1 => said.push_str(" (1 connection)"),
            open => {
                let _ = write!(said, " ({open} connections)");
            }
        }
    }
    said.push_str(" · :PortForward! PORT stops one");
    said
}

/// The host a forward goes to, as written after `:PortForward`: IPv6
/// addresses in brackets.
fn host(spec: &Spec) -> String {
    if spec.host.contains(':') {
        format!("[{}]", spec.host)
    } else {
        spec.host.clone()
    }
}

/// Why `asked` couldn't be forwarded, and what to do instead.
fn refusal(err: &AddError, asked: &Spec) -> String {
    let there = if asked.host == "localhost" {
        asked.port.to_string()
    } else {
        format!("{}:{}", host(asked), asked.port)
    };
    let instead = |free: Option<u16>| match free {
        Some(free) => format!(": :PortForward {free}:{there} forwards port {free} instead"),
        None => format!(": :PortForward LOCAL:{there} forwards another"),
    };
    match err {
        AddError::InUse { port, free } => {
            format!("Port {port} is in use on this machine{}", instead(*free))
        }
        AddError::Privileged(port) => {
            format!("Only root may listen on port {port} here{}", instead(None))
        }
        AddError::Forwarded(port) => {
            format!("Port {port} is forwarded already: :PortForward! {port} stops it")
        }
        AddError::Io { .. } => {
            let mut said = err.to_string();
            said.replace_range(..1, "C");
            said
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};

    use clackatoo_daemon::forward::Problem;
    use clackatoo_protocol::ForwardFailure;

    use super::*;

    fn spec(local: u16, host: &str, port: u16) -> Spec {
        Spec {
            local,
            host: host.into(),
            port,
        }
    }

    #[test]
    fn a_window_on_this_machine_has_nothing_to_forward_but_the_config_isnt_told() {
        let mut forwards = Forwards::new(|| {});
        let typed = Forwarding::Add(spec(0, "localhost", 80));
        let said = run(&mut forwards, typed, None).unwrap();
        assert!(said.contains("nothing to forward"), "{said}");
        let config = Forwarding::Config(spec(0, "localhost", 80));
        assert_eq!(run(&mut forwards, config, None), None);
        assert!(forwards.is_empty());
        let config = Forwarding::Config(spec(0, "localhost", 80));
        let said = run(&mut forwards, config, Some("desk")).unwrap();
        assert!(said.starts_with("Forwarding port "), "{said}");
    }

    #[test]
    fn forwards_are_added_listed_and_stopped_saying_so() {
        let mut forwards = Forwards::new(|| {});
        let added = run(
            &mut forwards,
            Forwarding::Add(spec(0, "box", 8080)),
            Some("desk"),
        )
        .unwrap();
        let local = forwards.list()[0].0.local;
        assert_eq!(
            added,
            format!("Forwarding port {local} here to box:8080 on desk")
        );
        assert_eq!(
            run(&mut forwards, Forwarding::List, Some("desk")).unwrap(),
            format!("Forwarding {local} → box:8080 · :PortForward! PORT stops one")
        );
        assert_eq!(
            run(&mut forwards, Forwarding::Stop(Some(local)), Some("desk")).unwrap(),
            format!("Stopped forwarding {local} → box:8080")
        );
        assert_eq!(
            run(&mut forwards, Forwarding::Stop(None), Some("desk")).unwrap(),
            "No ports are forwarded"
        );
        assert_eq!(
            run(&mut forwards, Forwarding::List, Some("desk")).unwrap(),
            "No ports are forwarded: :PortForward 8080 forwards one"
        );
        assert_eq!(
            listing(
                &[
                    (spec(8080, "localhost", 8080), 2),
                    (spec(3000, "::1", 80), 1)
                ],
                Some("desk")
            ),
            "Forwarding 8080 (2 connections) · 3000 → [::1]:80 (1 connection) · :PortForward! \
             PORT stops one"
        );
    }

    #[test]
    fn a_port_in_use_suggests_forwarding_a_free_one() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut forwards = Forwards::new(|| {});
        let said = run(
            &mut forwards,
            Forwarding::Add(spec(port, "localhost", 8080)),
            Some("desk"),
        )
        .unwrap();
        let start = format!("Port {port} is in use on this machine: :PortForward ");
        assert!(said.starts_with(&start), "{said}");
        assert!(said.contains(":8080 forwards port "), "{said}");
    }

    #[test]
    fn a_report_says_what_went_wrong_once_in_words() {
        let refused = Report {
            spec: spec(3000, "localhost", 8080),
            problem: Problem::Failed(ForwardFailure::Refused),
        };
        assert_eq!(
            report(&refused, "desk"),
            "Nothing is listening on localhost:8080 on desk, which port 3000 here forwards to"
        );
    }
}
