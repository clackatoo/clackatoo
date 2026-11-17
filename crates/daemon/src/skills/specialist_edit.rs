//! A specialist as Settings edits it: its settings as one JSON object,
//! with the same fields a `SPECIALIST.md`'s front matter has, whatever
//! agent it's for.

use clackatoo_protocol::SpecialistDefinition;
use serde_json::{Value, json};

use crate::specialists::Specialist;

/// `specialist` as Settings shows it, to be edited or copied. Fields it
/// leaves to their defaults are left out, but for those the form always
/// shows.
pub(crate) fn definition(specialist: &Specialist) -> SpecialistDefinition {
    let extra = &specialist.extra;
    let mut settings = json!({
        "description": specialist.description,
        "instructions": specialist.instructions,
        "when_to_use": extra.when_to_use,
        "user_invocable": extra.user_invocable,
        "agent_invocable": extra.agent_invocable,
        "limits": { "access": specialist.limits.name() },
    });
    if let Some(agent) = &specialist.agent {
        settings["agent"] = json!(agent);
    }
    if specialist.inherit_model {
        settings["model"] = json!("inherit");
    } else if !specialist.models.is_empty() {
        settings["model"] = by_agent(&specialist.models);
    }
    if !specialist.efforts.is_empty() {
        settings["effort"] = by_agent(&specialist.efforts);
    }
    let lists = [
        ("skills", &extra.skills),
        ("skillsets", &extra.skillsets),
        ("mcp_servers", &extra.mcp_servers),
        ("delegates_to", &extra.delegates_to),
    ];
    for (key, names) in lists {
        if let Some(names) = names {
            settings[key] = json!(names);
        }
    }
    if let Some(seconds) = extra.budget.seconds {
        settings["limits"]["seconds"] = json!(seconds);
    }
    if let Some(turns) = extra.budget.turns {
        settings["limits"]["turns"] = json!(turns);
    }
    if let Some(micros) = extra.budget.usd_micros {
        settings["limits"]["cost_usd"] = dollars(micros);
    }
    SpecialistDefinition {
        name: specialist.name.clone(),
        settings: settings.to_string(),
        saved: None,
        locked: None,
    }
}

/// `micros`, millionths of a dollar, as a number of dollars, exactly:
/// written out and read back, as dividing would round it.
fn dollars(micros: u64) -> Value {
    let written = format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000);
    Value::Number(written.parse().expect("a number, as written"))
}

/// A model or an effort for each agent, as a specialist's file has it:
/// one value, if it's the same for any agent, or else a table by agent.
fn by_agent(values: &[(String, String)]) -> Value {
    if let [(agent, value)] = values
        && agent == "*"
    {
        return json!(value);
    }
    let table = values
        .iter()
        .map(|(agent, value)| (agent.clone(), json!(value)));
    Value::Object(table.collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specialists::{Budget, Extras, Limits};

    #[test]
    fn a_specialist_is_shown_with_what_it_sets_and_no_more() {
        let plain = definition(&Specialist {
            name: "plain".into(),
            ..Specialist::default()
        });
        let settings: Value = serde_json::from_str(&plain.settings).unwrap();
        assert_eq!(settings["limits"], json!({ "access": "none" }));
        for unset in ["agent", "model", "effort", "skills", "delegates_to"] {
            assert!(settings.get(unset).is_none(), "{unset}");
        }

        let full = definition(&Specialist {
            name: "full".into(),
            limits: Limits::ReadOnly,
            models: vec![("*".into(), "haiku".into())],
            efforts: vec![
                ("claude".into(), "low".into()),
                ("codex".into(), "high".into()),
            ],
            extra: Extras {
                skills: Some(Vec::new()),
                budget: Budget {
                    seconds: Some(30),
                    turns: Some(4),
                    usd_micros: Some(1_500_000),
                },
                ..Extras::default()
            },
            ..Specialist::default()
        });
        let settings: Value = serde_json::from_str(&full.settings).unwrap();
        assert_eq!(settings["model"], "haiku");
        assert_eq!(
            settings["effort"],
            json!({ "claude": "low", "codex": "high" })
        );
        assert_eq!(settings["skills"], json!([]));
        let limits = json!({ "access": "read-only", "seconds": 30, "turns": 4, "cost_usd": 1.5 });
        assert_eq!(settings["limits"], limits);
    }
}
