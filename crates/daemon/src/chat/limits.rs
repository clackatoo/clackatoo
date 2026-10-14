//! An account's limits, as its agent says them, and the context a chat
//! fills, made safe to show.
//!
//! Claude Code's adapter says how much of a subscription's limits is used
//! in `usage_update`'s `_meta`, under `_claude/rateLimit`, once something
//! about one changes: `{status, rateLimitType, utilization, resetsAt}`,
//! one limit at a time, `utilization` from 0 to 1 and `resetsAt` in
//! seconds since the Unix epoch. Codex's adapter keeps its limits to its
//! own `/status`, so a Codex chat has none.
//!
//! The limits are the account's, not a chat's, so the daemon keeps them
//! per agent ([`Limits`]) and gives every chat of that agent the latest.
//! What an agent says is untrusted: percentages are kept from 0 to 100,
//! times to the years around now, and token counts to [`MOST_TOKENS`].

use agent_client_protocol_schema::v1::Meta;
use clackatoo_protocol::Limit;
use serde_json::Value;

/// Where Claude Code's adapter says a limit, in `usage_update`'s `_meta`.
const RATE_LIMIT: &str = "_claude/rateLimit";

/// The most tokens a context is taken to hold or have used: more is
/// nonsense, and is cut to it.
pub(crate) const MOST_TOKENS: u64 = 1 << 40;

/// The earliest a limit is taken to start again, in seconds since the
/// Unix epoch: 2020. An earlier time is nonsense, and isn't said.
const EARLIEST: f64 = 1_577_836_800.0;

/// The latest, likewise: 2100.
const LATEST: f64 = 4_102_444_800.0;

/// An account's limits, as its agent last said them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Over five hours.
    pub(crate) five_hour: Option<Limit>,
    /// Over a week.
    pub(crate) weekly: Option<Limit>,
}

/// Which of an account's limits an agent spoke of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    FiveHour,
    Weekly,
}

impl Limits {
    /// Takes in what `meta`, a `usage_update`'s, says of a limit, if it
    /// says something of the five hours' or the week's. Returns whether
    /// that changed them.
    pub(crate) fn heard(&mut self, meta: Option<&Meta>) -> bool {
        let Some((which, limit)) = meta.and_then(|meta| meta.get(RATE_LIMIT)).and_then(said) else {
            return false;
        };
        let slot = match which {
            Which::FiveHour => &mut self.five_hour,
            Which::Weekly => &mut self.weekly,
        };
        let changed = *slot != Some(limit);
        *slot = Some(limit);
        changed
    }
}

/// What Claude Code's adapter says of one limit, if it's the five hours'
/// or the week's and says how much of it is used. A limit reached is all
/// used, whatever it says. Others, of one model or of extra usage, aren't
/// shown.
fn said(info: &Value) -> Option<(Which, Limit)> {
    let which = match info.get("rateLimitType")?.as_str()? {
        "five_hour" => Which::FiveHour,
        "seven_day" => Which::Weekly,
        _ => return None,
    };
    let reached = info.get("status").and_then(Value::as_str) == Some("rejected");
    let used = if reached {
        100
    } else {
        percent(info.get("utilization")?.as_f64()?)?
    };
    let resets = (info.get("resetsAt").and_then(Value::as_f64))
        .filter(|seconds| (EARLIEST..=LATEST).contains(seconds))
        .map(|seconds| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "between 2020 and 2100, in milliseconds"
            )]
            let ms = (seconds * 1000.0) as u64;
            ms
        });
    Some((which, Limit { used, resets }))
}

/// A share from 0 to 1 as a percentage from 0 to 100, rounded: a share
/// past either end is that end. `None` for one that isn't a number.
fn percent(share: f64) -> Option<u8> {
    if !share.is_finite() {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "from 0 to 100"
    )]
    let percent = (share.clamp(0.0, 1.0) * 100.0).round() as u8;
    Some(percent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(info: &Value) -> Meta {
        let mut meta = Meta::new();
        meta.insert(RATE_LIMIT.to_owned(), info.clone());
        meta
    }

    fn heard(limits: &mut Limits, info: &Value) -> bool {
        limits.heard(Some(&meta(info)))
    }

    #[test]
    fn the_five_hours_and_the_weeks_limits_are_kept_apart() {
        let mut limits = Limits::default();
        assert!(heard(
            &mut limits,
            &serde_json::json!({ "status": "allowed", "rateLimitType": "five_hour",
                                 "utilization": 0.42, "resetsAt": 1_760_018_400 }),
        ));
        assert!(heard(
            &mut limits,
            &serde_json::json!({ "status": "allowed_warning", "rateLimitType": "seven_day",
                                 "utilization": 0.836 }),
        ));
        assert_eq!(
            limits,
            Limits {
                five_hour: Some(Limit {
                    used: 42,
                    resets: Some(1_760_018_400_000),
                }),
                weekly: Some(Limit {
                    used: 84,
                    resets: None,
                }),
            }
        );
        // Said again, the same: nothing changed.
        assert!(!heard(
            &mut limits,
            &serde_json::json!({ "status": "allowed_warning", "rateLimitType": "seven_day",
                                 "utilization": 0.836 }),
        ));
    }

    #[test]
    fn what_an_agent_says_of_its_limits_is_kept_to_sense() {
        let five = |info: Value| {
            let mut limits = Limits::default();
            heard(&mut limits, &info);
            limits.five_hour
        };
        // Past either end of its share, or reached without saying how much.
        let limit = |used, resets| Some(Limit { used, resets });
        assert_eq!(
            five(serde_json::json!({ "rateLimitType": "five_hour", "utilization": 7.5 })),
            limit(100, None)
        );
        assert_eq!(
            five(serde_json::json!({ "rateLimitType": "five_hour", "utilization": -3 })),
            limit(0, None)
        );
        assert_eq!(
            five(
                serde_json::json!({ "rateLimitType": "five_hour", "status": "rejected",
                                     "resetsAt": 1_760_018_400 })
            ),
            limit(100, Some(1_760_018_400_000))
        );
        // Starting again long ago, far off, or in milliseconds already.
        for resets in [
            serde_json::json!(-1),
            serde_json::json!(1e300),
            serde_json::json!(1_760_018_400_000_u64),
            serde_json::json!("soon"),
        ] {
            assert_eq!(
                five(
                    serde_json::json!({ "rateLimitType": "five_hour", "utilization": 0.5,
                                         "resetsAt": resets })
                ),
                limit(50, None)
            );
        }
        // Nothing said of how much, not a number, another limit, or not an
        // object at all: nothing heard.
        for info in [
            serde_json::json!({ "rateLimitType": "five_hour", "status": "allowed" }),
            serde_json::json!({ "rateLimitType": "five_hour", "utilization": "half" }),
            serde_json::json!({ "rateLimitType": "seven_day_opus", "utilization": 0.9 }),
            serde_json::json!({ "rateLimitType": 5, "utilization": 0.9 }),
            serde_json::json!([1, 2]),
            serde_json::json!(null),
        ] {
            assert_eq!(five(info.clone()), None, "{info}");
        }
        assert!(!Limits::default().heard(None));
        assert!(!Limits::default().heard(Some(&Meta::new())));
    }
}
