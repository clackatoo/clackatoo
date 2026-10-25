//! Google's account quota APIs, isolated in the bounded usage subprocess.
//! The runtime owns the refresh token; we neither rewrite it nor start a turn.
use super::Reading;
use clackatoo_protocol::UsageLimit;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

const ENDPOINTS: [&str; 2] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const MAX_BODY: u64 = 1024 * 1024;

// Never Debug: error messages and logs must not contain credentials.
#[derive(Deserialize)]
struct Credentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    project_id: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    SignIn,
    Unavailable,
}

impl From<ureq::Error> for Failure {
    fn from(_: ureq::Error) -> Self {
        Self::Unavailable
    }
}

pub(crate) fn read(profile: &Path) -> Reading {
    let result = credentials(profile).and_then(|credentials| {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(8)))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        probe(&credentials, &mut |url, token, body| {
            let response = agent.post(url);
            let response = if let Some(token) = token {
                response
                    .header("Authorization", format!("Bearer {token}"))
                    // Google's legacy hub identity omits Gemini/Argon quotas.
                    // Match our pinned ACP runtime and identify our host.
                    .header("User-Agent", client_identity())
                    .header("Content-Type", "application/json")
                    .send(body)
            } else {
                response
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .send(body)
            };
            let mut response = match response {
                Err(ureq::Error::StatusCode(401)) => return Err(Failure::SignIn),
                // OAuth invalid_grant: a revoked/expired refresh token must
                // clear the previous account's allowance, not keep it stale.
                Err(ureq::Error::StatusCode(400)) if url == TOKEN_URL => {
                    return Err(Failure::SignIn);
                }
                other => other?,
            };
            // max_redirects(0) returns the 3xx response; never follow it or
            // accept its body as a valid account reading.
            if !response.status().is_success() {
                return Err(Failure::Unavailable);
            }
            let bytes = response
                .body_mut()
                .with_config()
                .limit(MAX_BODY)
                .read_to_vec()?;
            serde_json::from_slice(&bytes).map_err(|_| Failure::Unavailable)
        })
    });
    match result {
        Ok(limits) => Reading {
            ok: true,
            limits,
            notice: None,
        },
        Err(Failure::SignIn) => Reading {
            ok: true,
            limits: Vec::new(),
            notice: Some(
                "Sign in with clackatoo agent login antigravity on the workspace's machine.".into(),
            ),
        },
        Err(Failure::Unavailable) => Reading {
            ok: false,
            limits: Vec::new(),
            notice: Some("Antigravity limits unavailable · refresh to retry.".into()),
        },
    }
}

fn client_identity() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        arch => arch,
    };
    format!(
        "antigravity/acp/{} (aidev_client; os_type=linux; arch={arch}; host_path=clackatoo/{}; proxy_client=antigravity/sdk)",
        crate::agents::ANTIGRAVITY_VERSION,
        env!("CARGO_PKG_VERSION")
    )
}

fn credentials(profile: &Path) -> Result<Credentials, Failure> {
    let file =
        std::fs::File::open(profile.join("antigravity-acp/acp_token.json")).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Failure::SignIn
            } else {
                Failure::Unavailable
            }
        })?;
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Unavailable)?;
    if bytes.len() > 65_536 {
        return Err(Failure::Unavailable);
    }
    let credentials: Credentials = serde_json::from_slice(&bytes).map_err(|_| Failure::SignIn)?;
    if [
        &credentials.client_id,
        &credentials.client_secret,
        &credentials.refresh_token,
    ]
    .iter()
    .any(|s| s.is_empty() || s.len() > 16_384)
    {
        return Err(Failure::SignIn);
    }
    Ok(credentials)
}

// HTTP is injected for deterministic tests; production only calls fixed Google
// URLs, never token_uri or an endpoint supplied by a token file or agent.
fn probe(
    credentials: &Credentials,
    request: &mut impl FnMut(&str, Option<&str>, &str) -> Result<Value, Failure>,
) -> Result<Vec<UsageLimit>, Failure> {
    let started = Instant::now();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &credentials.client_id)
        .append_pair("client_secret", &credentials.client_secret)
        .append_pair("refresh_token", &credentials.refresh_token)
        .append_pair("grant_type", "refresh_token")
        .finish();
    let refreshed = request(TOKEN_URL, None, &form)?;
    let token = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 16_384)
        .ok_or(Failure::Unavailable)?;
    let body = credentials
        .project_id
        .as_ref()
        .filter(|s| !s.is_empty())
        .map_or_else(|| json!({}), |project| json!({"project":project}))
        .to_string();
    let now = super::now();
    let mut limits = Vec::new();
    for operation in ["retrieveUserQuotaSummary", "fetchAvailableModels"] {
        for endpoint in ENDPOINTS {
            if started.elapsed() >= Duration::from_secs(20) {
                break;
            }
            match request(
                &format!("{endpoint}/v1internal:{operation}"),
                Some(token),
                &body,
            ) {
                Ok(data) => {
                    let found = if operation == "retrieveUserQuotaSummary" {
                        summary(&data, now)
                    } else {
                        models(&data)
                    };
                    for limit in found {
                        merge(&mut limits, limit);
                    }
                }
                Err(Failure::SignIn) => return Err(Failure::SignIn),
                Err(Failure::Unavailable) => {}
            }
        }
        if !limits.is_empty() {
            return Ok(limits);
        }
    }
    Err(Failure::Unavailable)
}

fn merge(limits: &mut Vec<UsageLimit>, limit: UsageLimit) {
    if let Some(old) = limits.iter_mut().find(|old| old.id == limit.id) {
        if limit.used > old.used || (limit.used == old.used && old.resets.is_none()) {
            *old = limit;
        }
    } else if limits.len() < 32 {
        limits.push(limit);
    }
}

fn quota(id: &str, label: &str, value: &Value) -> Option<UsageLimit> {
    let remaining = value.get("remainingFraction")?.as_f64()?.clamp(0.0, 1.0);
    if !remaining.is_finite() {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped finite percentage"
    )]
    let used = ((1.0 - remaining) * 100.0).round() as u8;
    let resets = value
        .get("resetTime")
        .and_then(Value::as_str)
        .and_then(|time| time.parse::<jiff::Timestamp>().ok())
        .and_then(|time| u64::try_from(time.as_millisecond()).ok())
        .filter(|ms| (1_577_836_800_000..=4_102_444_800_000).contains(ms));
    Some(UsageLimit {
        id: super::clean(id),
        label: super::clean(label),
        used,
        resets,
    })
}

fn summary(data: &Value, now: u64) -> Vec<UsageLimit> {
    let mut limits = Vec::new();
    let groups = data.get("groups").and_then(Value::as_array);
    let grouped = groups.is_some_and(|groups| {
        groups.iter().take(32).any(|group| {
            group
                .get("buckets")
                .and_then(Value::as_array)
                .is_some_and(|b| !b.is_empty())
        })
    });
    if grouped {
        for group in groups.into_iter().flatten().take(32) {
            buckets(&mut limits, group, now);
        }
    } else {
        buckets(&mut limits, data, now);
    }
    limits
}

fn buckets(limits: &mut Vec<UsageLimit>, group: &Value, now: u64) {
    for bucket in group
        .get("buckets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(128)
    {
        if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(id) = bucket
            .get("bucketId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let family = if id.starts_with("gemini-argon-") {
            "Gemini 4 Argon"
        } else if id.starts_with("gemini-") {
            "Gemini"
        } else if id.starts_with("3p-") {
            "Claude & GPT (shared)"
        } else {
            group
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(id)
        };
        let (window, duration) = match bucket.get("window").and_then(Value::as_str) {
            Some("weekly") => ("Weekly", Some(7 * 24 * 60 * 60 * 1000)),
            Some("5h") => ("5-hour", Some(5 * 60 * 60 * 1000)),
            _ => (
                bucket
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("Quota"),
                None,
            ),
        };
        if let Some(mut limit) = quota(id, &format!("{family} · {window}"), bucket) {
            // Google reports now+window for untouched buckets. Hide only that
            // synthetic countdown, retaining measured resets even at 100%.
            if bucket.get("remainingFraction").and_then(Value::as_f64) == Some(1.0)
                && bucket
                    .get("description")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.trim().is_empty())
                && duration.zip(limit.resets).is_some_and(|(duration, reset)| {
                    reset.abs_diff(now.saturating_add(duration)) <= 60_000
                })
            {
                limit.resets = None;
            }
            merge(limits, limit);
        }
    }
}

fn models(data: &Value) -> Vec<UsageLimit> {
    let mut limits = Vec::new();
    for (id, model) in data
        .get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .take(256)
    {
        let provider = model.get("modelProvider").and_then(Value::as_str);
        let family = if id.starts_with("gemini-4-argon") {
            "Gemini 4 Argon"
        } else if id.starts_with("gemini-") || provider == Some("MODEL_PROVIDER_GOOGLE") {
            "Gemini"
        } else if id.starts_with("claude-") || provider == Some("MODEL_PROVIDER_ANTHROPIC") {
            "Claude"
        } else if id.starts_with("gpt-") || provider == Some("MODEL_PROVIDER_OPENAI") {
            "GPT"
        } else {
            continue;
        };
        for (key, label) in [
            ("quotaInfo", "Reported quota"),
            ("weeklyQuotaInfo", "Weekly"),
        ] {
            if let Some(limit) = model.get(key).and_then(|value| {
                quota(
                    &format!("{family}:{key}"),
                    &format!("{family} · {label}"),
                    value,
                )
            }) {
                merge(&mut limits, limit);
            }
        }
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_identity_and_argon_quotas_follow_the_native_runtime() {
        assert!(client_identity().starts_with(&format!(
            "antigravity/acp/{} ",
            crate::agents::ANTIGRAVITY_VERSION
        )));
        assert!(client_identity().contains("host_path=clackatoo/"));
        assert!(!client_identity().contains("arch=x64"));
        let limits = summary(
            &json!({"buckets":[
                {"bucketId":"gemini-weekly","window":"weekly","remainingFraction":0.9},
                {"bucketId":"gemini-argon-wtu-weekly","window":"weekly","remainingFraction":0.15}
            ]}),
            0,
        );
        assert_eq!(limits[0].label, "Gemini · Weekly");
        assert_eq!(limits[1].label, "Gemini 4 Argon · Weekly");
        assert_eq!(limits[1].used, 85);
        let fallback = models(&json!({"models":{
            "gemini-3.8-flash":{"weeklyQuotaInfo":{"remainingFraction":0.9}},
            "gemini-4-argon":{"weeklyQuotaInfo":{"remainingFraction":0.15}}
        }}));
        assert_eq!(fallback.len(), 2);
        assert!(
            fallback
                .iter()
                .any(|l| l.label == "Gemini 4 Argon · Weekly" && l.used == 85)
        );
    }

    #[test]
    fn missing_summary_falls_back_and_a_broken_endpoint_does_not_hide_the_other() {
        let credentials = Credentials {
            client_id: "a".into(),
            client_secret: "b".into(),
            refresh_token: "c".into(),
            project_id: None,
        };
        let result = probe(&credentials, &mut |url, _, _| {
            if url == TOKEN_URL {
                return Ok(json!({"access_token":"access"}));
            }
            if url.contains("daily-") {
                return Err(Failure::Unavailable);
            }
            if url.ends_with("retrieveUserQuotaSummary") {
                return Ok(json!({}));
            }
            assert!(url.ends_with("fetchAvailableModels"));
            Ok(json!({"models":{"gemini-3.8-flash":{"quotaInfo":{"remainingFraction":0.9}}}}))
        })
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].used, 10);
    }

    #[test]
    fn buckets_merge_without_inventing_an_untouched_reset() {
        let now = 1_792_922_400_000;
        let data = json!({"groups":[{"buckets":[
            {"bucketId":"gemini-5h","window":"5h","remainingFraction":0.84,"resetTime":"2026-10-25T15:00:00Z"},
            {"bucketId":"3p-5h","window":"5h","remainingFraction":1,"resetTime":"2026-10-25T15:00:00Z"},
            {"bucketId":"3p-weekly","window":"weekly","remainingFraction":1,"resetTime":"2026-11-01T10:00:00Z","description":"Measured reset"},
            {"bucketId":"disabled","disabled":true,"remainingFraction":0},
            {"bucketId":"bad","remainingFraction":"half"}
        ]}]});
        let limits = summary(&data, now);
        assert_eq!(limits.len(), 3);
        assert_eq!(limits[0].used, 16);
        assert!(limits[0].resets.is_some());
        assert_eq!(limits[1].label, "Claude & GPT (shared) · 5-hour");
        assert_eq!(limits[1].resets, None);
        assert!(limits[2].resets.is_some());
        let mut changed = data;
        changed["groups"][0]["buckets"][1]["resetTime"] = json!("2026-10-25T14:00:00Z");
        assert!(summary(&changed, now)[1].resets.is_some());
    }

    #[test]
    fn model_fallback_uses_the_most_used_quota_without_guessing_its_cadence() {
        let limits = models(&json!({"models":{
            "gemini-a":{"quotaInfo":{"remainingFraction":0.9}},
            "gemini-b":{"quotaInfo":{"remainingFraction":0.4,"resetTime":"nonsense"},"weeklyQuotaInfo":{"remainingFraction":-1}},
            "claude-a":{"quotaInfo":{"remainingFraction":2}},
            "other":{"quotaInfo":{"remainingFraction":0}}
        }}));
        assert_eq!(limits.len(), 3);
        let gemini = limits.iter().find(|l| l.id == "Gemini:quotaInfo").unwrap();
        assert_eq!(gemini.used, 60);
        assert_eq!(gemini.label, "Gemini · Reported quota");
        assert_eq!(gemini.resets, None);
    }

    #[test]
    fn refreshes_only_google_then_merges_both_endpoints_without_a_model_turn() {
        let credentials = Credentials {
            client_id: "client".into(),
            client_secret: "secret&".into(),
            refresh_token: "refresh+".into(),
            project_id: Some("project".into()),
        };
        let mut calls = 0;
        let result = probe(&credentials, &mut |url, token, body| {
            calls += 1;
            if calls == 1 {
                assert_eq!(url, TOKEN_URL);
                assert_eq!(token, None);
                assert!(body.contains("client_secret=secret%26"));
                return Ok(json!({"access_token":"access"}));
            }
            assert_eq!(token, Some("access"));
            assert_eq!(serde_json::from_str::<Value>(body).unwrap(), json!({"project":"project"}));
            assert_eq!(url, format!("{}/v1internal:retrieveUserQuotaSummary", ENDPOINTS[calls-2]));
            Ok(json!({"buckets":[{"bucketId":"gemini-5h","window":"5h","remainingFraction":if calls==2 {0.8} else {0.5}}]}))
        }).unwrap();
        assert_eq!(calls, 3);
        assert_eq!(result[0].used, 50);
    }

    #[test]
    fn absent_or_revoked_login_clears_old_limits_but_network_failure_keeps_them() {
        let temp = tempfile::tempdir().unwrap();
        let reading = read(temp.path());
        assert!(reading.ok);
        assert!(reading.limits.is_empty());
        assert!(reading.notice.unwrap().contains("Sign in"));
        let credentials = Credentials {
            client_id: "a".into(),
            client_secret: "b".into(),
            refresh_token: "c".into(),
            project_id: None,
        };
        assert!(matches!(
            probe(&credentials, &mut |_, _, _| Err(Failure::SignIn)),
            Err(Failure::SignIn)
        ));
        assert!(matches!(
            probe(&credentials, &mut |_, _, _| Err(Failure::Unavailable)),
            Err(Failure::Unavailable)
        ));
    }
}
