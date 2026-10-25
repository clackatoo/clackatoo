//! Read-only subscription probes on one bounded worker, paced across windows.
//! SDK/CLI processes own authentication; credentials never enter our protocol.
use crate::agents::{AgentConfig, Launch, install_dir};
use crate::group::Group;
use clackatoo_protocol::{ProviderUsage, UsageLimit};
use serde::Deserialize;
use std::io::{ErrorKind, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct Probe {
    agent: String,
    node: PathBuf,
    entry: PathBuf,
}
#[derive(Debug, Deserialize)]
struct Reading {
    ok: bool,
    limits: Vec<UsageLimit>,
    notice: Option<String>,
}
#[derive(Debug)]
struct Worker {
    receive: mpsc::Receiver<(String, Reading)>,
    cancel: Arc<AtomicBool>,
    group: Arc<Mutex<Option<Arc<Group>>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(group) = self.group.lock().unwrap().as_ref() {
            group.signal(rustix::process::Signal::KILL);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Cache {
    pub(crate) reports: Vec<ProviderUsage>,
    requested: Option<Instant>,
    worker: Option<Worker>,
    messages: u8,
}
impl Cache {
    pub(crate) fn stop(&mut self) {
        self.worker = None;
    }

    /// Warm the cache after the first message, then every three completed
    /// messages across chats. Slow conversations also refresh on the first
    /// completion after five minutes. No timer or idle polling is needed.
    pub(crate) fn after_message(&mut self, config: &AgentConfig, waker: Arc<mio::Waker>) -> bool {
        self.messages = self.messages.saturating_add(1);
        if self.messages >= 3
            || self
                .requested
                .is_none_or(|at| at.elapsed() >= Duration::from_mins(5))
        {
            self.refresh(config, true, waker)
        } else {
            false
        }
    }

    /// A refresh never starts a model turn. Manual refreshes also have a floor.
    pub(crate) fn refresh(
        &mut self,
        config: &AgentConfig,
        force: bool,
        waker: Arc<mio::Waker>,
    ) -> bool {
        if self.worker.is_some()
            || self
                .requested
                .is_some_and(|at| at.elapsed() < Duration::from_secs(if force { 60 } else { 300 }))
        {
            return false;
        }
        self.requested = Some(Instant::now());
        self.messages = 0;
        let mut probes = Vec::new();
        let old = std::mem::take(&mut self.reports);
        for spec in config
            .all()
            .into_iter()
            .filter(|spec| config.installed(spec))
            .take(32)
        {
            let mut report = old
                .iter()
                .find(|p| p.agent == spec.name)
                .cloned()
                .unwrap_or_else(|| ProviderUsage {
                    agent: spec.name.clone(),
                    title: spec.title.clone(),
                    ..ProviderUsage::default()
                });
            report.title.clone_from(&spec.title);
            let probe = locate_probe(config, &spec);
            if probe.is_none() {
                report.limits.clear();
                report.checked = 0;
            }
            report.refreshing = probe.is_some();
            report.notice = if probe.is_some() {
                None
            } else {
                Some("This adapter does not expose subscription limits.".into())
            };
            if let Some(probe) = probe {
                probes.push(probe);
            }
            self.reports.push(report);
        }
        if probes.is_empty() {
            return true;
        }
        let (send, receive) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let group = Arc::new(Mutex::new(None));
        let (cancelled, running) = (cancel.clone(), group.clone());
        let thread = std::thread::Builder::new()
            .name("provider-usage".into())
            .spawn(move || {
                for probe in probes {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let reading =
                        probe_read(&probe, &cancelled, &running).unwrap_or_else(|| Reading {
                            ok: false,
                            limits: Vec::new(),
                            notice: Some("Usage unavailable · check sign-in or try later.".into()),
                        });
                    if send.send((probe.agent, reading)).is_err() {
                        break;
                    }
                    let _ = waker.wake();
                }
            });
        match thread {
            Ok(thread) => {
                self.worker = Some(Worker {
                    receive,
                    cancel,
                    group,
                    thread: Some(thread),
                });
            }
            Err(_) => {
                for report in &mut self.reports {
                    report.refreshing = false;
                    report.notice = Some("Could not start usage refresh.".into());
                }
            }
        }
        true
    }

    pub(crate) fn take(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let mut changed = false;
        for (agent, reading) in worker.receive.try_iter() {
            if let Some(report) = self.reports.iter_mut().find(|p| p.agent == agent) {
                report.refreshing = false;
                // A failed probe keeps its last successful reading and its age.
                if reading.ok {
                    report.limits = reading.limits;
                    report.checked = now();
                }
                report.notice = reading.notice;
                changed = true;
            }
        }
        if self.reports.iter().all(|report| !report.refreshing) {
            self.worker = None;
        }

        changed
    }
}

fn locate_probe(config: &AgentConfig, spec: &crate::agents::AgentSpec) -> Option<Probe> {
    if let (Launch::Known(known), Some(dir), Some(node)) =
        (&spec.launch, &config.dir, config.usage_node())
    {
        let root = install_dir(dir, known)
            .join(known.adapter.program)
            .join("node_modules");
        let entry = match known.name {
            "claude" => Some(root.join("@anthropic-ai/claude-agent-sdk/sdk.mjs")),
            "codex" => Some(root.join("@openai/codex/bin/codex.js")),
            _ => None,
        };
        entry.filter(|p| p.is_file()).map(|entry| Probe {
            agent: spec.name.clone(),
            node,
            entry,
        })
    } else {
        None
    }
}

fn now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn probe_read(
    probe: &Probe,
    cancel: &AtomicBool,
    running: &Mutex<Option<Arc<Group>>>,
) -> Option<Reading> {
    let mut child = Command::new(&probe.node)
        .args([
            "--input-type=module",
            "-e",
            include_str!("provider_usage/probe.mjs"),
        ])
        .arg(&probe.agent)
        .arg(&probe.entry)
        .current_dir(std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).ok()?)?;
    let group = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
        .ok()
        .map(|fd| Arc::new(Group::new(fd)));
    running.lock().unwrap().clone_from(&group);
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let flags = rustix::fs::fcntl_getfl(&stdout).ok()?;
        rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK).ok()?;
        let start = Instant::now();
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(30) {
                return None;
            }
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.len() > 65536 {
                        return None;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
        let mut reading: Reading = serde_json::from_slice(&bytes).ok()?;
        reading.limits.truncate(32);
        for limit in &mut reading.limits {
            limit.used = limit.used.min(100);
            limit.id = clean(&limit.id);
            limit.label = clean(&limit.label);
        }
        reading.notice = reading.notice.map(|s| clean(&s));
        Some(reading)
    })();
    if let Some(group) = group {
        group.signal(rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    *running.lock().unwrap() = None;
    result
}
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(agent: &str, source: &str) -> Option<Reading> {
        let node = crate::agents::AgentConfig::default().usage_node()?;
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("mock.mjs");
        std::fs::write(&entry, source).unwrap();
        Some(
            probe_read(
                &Probe {
                    agent: agent.into(),
                    node,
                    entry,
                },
                &AtomicBool::new(false),
                &Mutex::new(None),
            )
            .expect("bounded probe"),
        )
    }

    #[test]
    fn claude_reads_quota_without_a_prompt_tools_or_hooks() {
        let Some(reading) = run(
            "claude",
            r"
            import assert from 'node:assert/strict';
            export function query({prompt, options}) {
                assert.equal(options.persistSession, false);
                assert.equal(options.settings.disableAllHooks, true);
                assert.deepEqual(options.tools, []);
                assert.deepEqual(options.mcpServers, {});
                assert.equal(options.strictMcpConfig, true);
                let initialized = false;
                const next = prompt.next();
                return {
                    async initializationResult() { initialized = true; },
                    async usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET(opts) {
                        assert(initialized); assert(opts.skipBehaviors);
                        return {rate_limits_available: true, rate_limits: {
                            five_hour: {utilization: 22.4, resets_at: '2026-10-25T10:00:00Z'},
                            seven_day: {utilization: 105},
                            seven_day_sonnet: {utilization: 'bad'},
                            model_scoped: [{display_name: 'Model', utilization: 31}],
                            extra_usage: {is_enabled: true, monthly_limit: 100, used_credits: 12}
                        }};
                    },
                    close() { next.then(({done}) => assert(done)); }
                };
            }
        ",
        ) else {
            return;
        };
        assert!(reading.ok);
        assert_eq!(
            reading
                .limits
                .iter()
                .map(|w| (w.label.as_str(), w.used))
                .collect::<Vec<_>>(),
            vec![
                ("5-hour", 22),
                ("Weekly", 100),
                ("Weekly · Model", 31),
                ("Monthly spend", 12)
            ]
        );
        assert_eq!(reading.limits[0].resets, Some(1_792_922_400_000));
        assert!(reading.limits[1].resets.is_none());
    }

    #[test]
    fn codex_reads_every_window_without_starting_a_thread_or_turn() {
        let Some(reading) = run(
            "codex",
            r"
            import {createInterface} from 'node:readline';
            import assert from 'node:assert/strict';
            let initialized = false;
            for await (const line of createInterface({input: process.stdin})) {
                const msg = JSON.parse(line);
                if (msg.method === 'initialized') { initialized = true; continue; }
                let result;
                if (msg.method === 'initialize') result = {};
                else {
                    assert(initialized); assert.equal(msg.method, 'account/rateLimits/read');
                    result = {rateLimitsByLimitId: {
                        codex: {primary: {windowDurationMins: 300, usedPercent: 41, resetsAt: 1792922400}, secondary: {windowDurationMins: 10080, usedPercent: 17}},
                        fast: {limitName: 'Fast', primary: {usedPercent: 9, windowDurationMins: 60}}
                    }};
                }
                console.log(JSON.stringify({id: msg.id, result}));
            }
        ",
        ) else {
            return;
        };
        assert!(reading.ok);
        assert_eq!(
            reading
                .limits
                .iter()
                .map(|w| (w.label.as_str(), w.used))
                .collect::<Vec<_>>(),
            vec![("5-hour", 41), ("7-day", 17), ("Fast · 1-hour", 9)]
        );
    }

    #[test]
    fn failures_keep_the_last_reading_but_a_successful_unavailable_read_clears_it() {
        let (send, receive) = mpsc::channel();
        let mut cache = Cache {
            reports: vec![ProviderUsage {
                agent: "claude".into(),
                checked: 123,
                refreshing: true,
                limits: vec![UsageLimit {
                    id: "week".into(),
                    label: "Weekly".into(),
                    used: 50,
                    resets: None,
                }],
                ..ProviderUsage::default()
            }],
            worker: Some(Worker {
                receive,
                cancel: Arc::new(AtomicBool::new(false)),
                group: Arc::new(Mutex::new(None)),
                thread: None,
            }),
            ..Cache::default()
        };
        send.send((
            "claude".into(),
            Reading {
                ok: false,
                limits: vec![],
                notice: Some("Unavailable".into()),
            },
        ))
        .unwrap();
        assert!(cache.take());
        assert_eq!(cache.reports[0].checked, 123);
        assert_eq!(cache.reports[0].limits[0].used, 50);
        assert!(cache.worker.is_none());
        let (send, receive) = mpsc::channel();
        cache.worker = Some(Worker {
            receive,
            cancel: Arc::new(AtomicBool::new(false)),
            group: Arc::new(Mutex::new(None)),
            thread: None,
        });
        cache.reports[0].refreshing = true;
        send.send((
            "claude".into(),
            Reading {
                ok: true,
                limits: vec![],
                notice: Some("No subscription".into()),
            },
        ))
        .unwrap();
        assert!(cache.take());
        assert!(cache.reports[0].limits.is_empty());
        assert!(cache.reports[0].checked > 123);
    }
    #[test]
    fn message_refreshes_warm_count_coalesce_and_retry_after_the_floor() {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let config = AgentConfig {
            dir: None,
            ..AgentConfig::default()
        };
        let mut cache = Cache::default();
        cache.after_message(&config, waker.clone());
        assert!(
            cache.requested.is_some(),
            "first completion warms the cache"
        );
        assert_eq!(cache.messages, 0);
        cache.requested = Instant::now().checked_sub(Duration::from_secs(61));
        let old = cache.requested;
        for _ in 0..2 {
            cache.after_message(&config, waker.clone());
        }
        assert_eq!(cache.requested, old, "wait for three messages");
        cache.after_message(&config, waker.clone());
        assert_ne!(cache.requested, old);
        let recent = cache.requested;
        for _ in 0..300 {
            cache.after_message(&config, waker.clone());
        }
        assert_eq!(
            cache.requested, recent,
            "fast messages cannot bypass pacing"
        );
        cache.requested = old;
        cache.after_message(&config, waker.clone());
        assert_ne!(
            cache.requested, old,
            "deferred count retries on later activity"
        );
        assert_eq!(cache.messages, 0);
        cache.requested = Instant::now().checked_sub(Duration::from_secs(301));
        let stale = cache.requested;
        cache.after_message(&config, waker.clone());
        assert_ne!(cache.requested, stale, "slow conversations refresh too");

        let (_send, receive) = mpsc::channel();
        cache.worker = Some(Worker {
            receive,
            cancel: Arc::new(AtomicBool::new(false)),
            group: Arc::new(Mutex::new(None)),
            thread: None,
        });
        cache.requested = old;
        for _ in 0..3 {
            cache.after_message(&config, waker.clone());
        }
        assert_eq!(cache.requested, old, "one probe at a time");
        assert_eq!(cache.messages, 3);
        cache.stop();
        cache.after_message(&config, waker);
        assert_ne!(cache.requested, old);
    }

    #[test]
    fn refreshes_are_shared_and_paced_even_when_forced() {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let config = AgentConfig {
            dir: None,
            ..AgentConfig::default()
        };
        let mut cache = Cache::default();
        cache.refresh(&config, false, waker.clone());
        let first = cache.requested;
        cache.refresh(&config, true, waker.clone());
        assert_eq!(cache.requested, first);
        cache.requested = Instant::now().checked_sub(Duration::from_secs(61));
        let old = cache.requested;
        cache.refresh(&config, false, waker.clone());
        assert_eq!(cache.requested, old);
        cache.refresh(&config, true, waker);
        assert_ne!(cache.requested, old);
    }
}
