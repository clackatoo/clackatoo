//! Provider-independent editor representation.
use crate::specialists::Specialist;
use clackatoo_protocol::SpecialistDefinition;

pub(crate) fn definition(specialist: &Specialist) -> SpecialistDefinition {
    use serde_json::json;
    let s = specialist;
    let mut v = json!({"description":s.description,"instructions":s.instructions,
            "when_to_use":s.extra.when_to_use,"user_invocable":s.extra.user_invocable,
            "agent_invocable":s.extra.agent_invocable,"limits":{"access":s.limits.name()}});
    if let Some(agent) = &s.agent {
        v["agent"] = json!(agent);
    }
    if s.inherit_model {
        v["model"] = json!("inherit");
    } else if !s.models.is_empty() {
        v["model"] = pairs_json(&s.models);
    }
    if !s.efforts.is_empty() {
        v["effort"] = pairs_json(&s.efforts);
    }
    for (key, values) in [
        ("skills", &s.extra.skills),
        ("skillsets", &s.extra.skillsets),
        ("delegates_to", &s.extra.delegates_to),
    ] {
        if let Some(values) = values {
            v[key] = json!(values);
        }
    }
    if let Some(n) = s.extra.budget.seconds {
        v["limits"]["seconds"] = json!(n);
    }
    if let Some(n) = s.extra.budget.turns {
        v["limits"]["turns"] = json!(n);
    }
    if let Some(n) = s.extra.budget.usd_micros {
        v["limits"]["cost_usd"] = serde_json::Value::Number(
            format!("{}.{:06}", n / 1_000_000, n % 1_000_000)
                .parse()
                .expect("bounded positive USD amount"),
        );
    }
    SpecialistDefinition {
        name: s.name.clone(),
        settings: v.to_string(),
        saved: None,
        locked: None,
    }
}

fn pairs_json(pairs: &[(String, String)]) -> serde_json::Value {
    if let [(any, value)] = pairs
        && any == "*"
    {
        return serde_json::json!(value);
    }
    serde_json::Value::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::json!(v)))
            .collect(),
    )
}
