//! The devices paired with the daemon's web app, as a window sees them:
//! those asking to pair, asked about in the bar until a window answers,
//! `:Devices`, which lists them and answers, and telling the daemon
//! whether the window's user is there, so it knows when a phone is to be
//! notified instead.
//!
//! A device asking shows a code, and the bar never does: it's allowed by
//! typing the code it shows, `:Devices allow CODE`, so only someone who
//! can see the device can let it in. Nothing in the bar takes a key, so a
//! question can't be answered by typing meant for a terminal.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use clackatoo_protocol::{ClientMessage, Device, PairingState, Request, Response};

use super::{Asked, Desk};
use crate::chrome::Status;
use crate::command::DevicesCommand;

/// How often a window in use says again that its user is there: well
/// within the five minutes the daemon takes it to last.
const PRESENT_EVERY: Duration = Duration::from_mins(1);

impl Desk {
    /// Pairing now stands as `pairing` says. Returns whether the window
    /// must be redrawn.
    pub(crate) fn pairing_asked(&mut self, pairing: PairingState) -> bool {
        let changed = self.pairing != pairing;
        self.pairing = pairing;
        changed
    }

    /// The question about the devices asking to pair, or that pairing
    /// with the password is locked, for the bar, while the command line
    /// isn't open.
    pub(super) fn pairing_status(&self) -> Option<Status> {
        if self.input.is_some() {
            return None;
        }
        let text = match self.pairing.asking.as_slice() {
            [] if self.pairing.locked => "Too many wrong passwords: pairing with the password \
                                          is locked until :Devices unlock"
                .to_owned(),
            [] => return None,
            [ask] => format!(
                "{} ({}) asks to control clackatoo: :Devices allow and the code it shows, \
                 or :Devices deny",
                ask.name, ask.from
            ),
            asks => format!(
                "{} devices ask to control clackatoo: :Devices allow and the code one shows, \
                 or :Devices deny",
                asks.len()
            ),
        };
        Some(Status::Typing {
            label: None,
            text,
            choices: Vec::new(),
        })
    }

    /// Does what `:Devices` says.
    pub(super) fn devices_command(&mut self, command: DevicesCommand) {
        let request = match command {
            DevicesCommand::List => Request::Devices,
            DevicesCommand::Allow(code) => Request::AnswerPairing { code: Some(code) },
            DevicesCommand::Deny => Request::AnswerPairing { code: None },
            DevicesCommand::Unlock => Request::UnlockPairing,
            DevicesCommand::Revoke(device) => Request::RevokeDevice { device },
        };
        self.request(request, Asked::Devices);
    }

    /// The daemon's answer to something `:Devices` asked: what to say in
    /// the bar.
    pub(super) fn devices_answered(&mut self, response: Response) -> bool {
        self.message = Some(match response {
            Response::Devices(devices) => listing(&devices, now_millis()),
            Response::Text(said) => said,
            Response::Done => "Unpaired the device".to_owned(),
            Response::Error(err) => err.to_string(),
            other => format!("the daemon answered {other:?}"),
        });
        true
    }

    /// Tells the daemon whether the window's user is there.
    pub(super) fn say_present(&mut self, here: bool) {
        if !self.connected {
            return;
        }
        self.present_said = here.then(Instant::now);
        self.outbox.push(ClientMessage::Present { here });
    }

    /// The window's user did something: the daemon hears they're there,
    /// if it hasn't lately.
    pub(crate) fn user_active(&mut self) {
        let lately = (self.present_said).is_some_and(|said| said.elapsed() < PRESENT_EVERY);
        if !self.unfocused && !lately {
            self.say_present(true);
        }
    }
}

/// The devices, for the bar: each with its number, and when it was last
/// used.
fn listing(devices: &[Device], now: u64) -> String {
    if devices.is_empty() {
        return "No devices are paired: open the web app on one to pair it".to_owned();
    }
    let each: Vec<String> = (devices.iter())
        .map(|device| {
            let mut said = format!("{} {}", device.id, device.name);
            if device.connected {
                said.push_str(" (connected");
            } else {
                let _ = write!(said, " (used {}", ago(now.saturating_sub(device.used)));
            }
            if device.notifications {
                said.push_str(", notified");
            }
            said.push(')');
            said
        })
        .collect();
    format!(
        "Devices: {} · :Devices revoke N unpairs one",
        each.join(" · ")
    )
}

/// How long `millis` milliseconds is, roughly, as "3 days ago".
fn ago(millis: u64) -> String {
    let minutes = millis / 60_000;
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match minutes {
        0 => "just now".to_owned(),
        1..60 => plural(minutes, "minute"),
        60..1440 => plural(minutes / 60, "hour"),
        _ => plural(minutes / 1440, "day"),
    }
}

/// Now, in milliseconds since the Unix epoch.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u64, used: u64, connected: bool) -> Device {
        Device {
            id,
            name: format!("phone {id}"),
            paired: 0,
            used,
            login: None,
            notifications: id == 1,
            connected,
        }
    }

    #[test]
    fn devices_are_listed_with_their_numbers() {
        let day = 24 * 60 * 60 * 1000;
        let listed = listing(&[device(1, 0, true), device(2, 0, false)], 3 * day);
        assert_eq!(
            listed,
            "Devices: 1 phone 1 (connected, notified) · 2 phone 2 (used 3 days ago) · \
             :Devices revoke N unpairs one"
        );
        assert!(listing(&[], 0).starts_with("No devices"));
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(60_000), "1 minute ago");
        assert_eq!(ago(2 * 60 * 60 * 1000), "2 hours ago");
    }
}
