//! Background work snapshots, coalesced for native windows. No polling while idle.

use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Error, Event, Response, WorkAction, wire};
use mio::{Token, Waker};

use super::{Daemon, Source};
use crate::work::Scheduler;

const EVERY: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(super) struct Work {
    pub(super) scheduler: Scheduler,
    revision: u64,
    told: Option<Instant>,
    pub(super) due: Option<Instant>,
}

impl Work {
    pub(super) fn new(waker: Arc<Waker>) -> Self {
        Self {
            scheduler: Scheduler::new(move || {
                let _ = waker.wake();
            }),
            revision: 0,
            told: None,
            due: None,
        }
    }
}

impl Daemon {
    fn can_see_work(&self, token: Token) -> bool {
        matches!(self.sources.get(token.0), Some(Source::Client(client))
            if client.watching_workspaces && !client.finished && client.wire >= wire::WORK)
            && self.not_a_window(token).is_none()
    }

    pub(super) fn serve_work(
        &mut self,
        token: Token,
        action: WorkAction,
    ) -> Result<Response, Error> {
        if self.not_a_window(token).is_some() {
            return Err(Error::Other(
                "Background work is controlled in a clackatoo window.".into(),
            ));
        }
        match action {
            WorkAction::List => {}
            WorkAction::Cancel(id) => self.work.scheduler.cancel(id).map_err(Error::Other)?,
            WorkAction::Dismiss(id) => self.work.scheduler.dismiss(id).map_err(Error::Other)?,
        }
        Ok(Response::Work(self.work.scheduler.snapshot().1))
    }

    pub(super) fn take_work(&mut self) {
        let revision = self.work.scheduler.revision();
        if revision == self.work.revision {
            self.work.due = None;
            return;
        }
        let now = Instant::now();
        let due = self.work.told.map_or(now, |told| told + EVERY);
        if due > now {
            self.work.due = Some(due);
            return;
        }
        self.work.revision = revision;
        self.work.told = Some(now);
        self.work.due = None;
        let items = self.work.scheduler.snapshot().1;
        let tokens: Vec<_> = self
            .sources
            .iter()
            .filter_map(|(key, _)| self.can_see_work(Token(key)).then_some(Token(key)))
            .collect();
        for token in tokens {
            self.send_event(token, Event::Work(items.clone()));
        }
    }
}
