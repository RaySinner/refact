use serde_json::{json, Value};

use crate::quota::{
    ProviderQuotaCache, ProviderQuotaFact, ProviderQuotaFactValue, ProviderQuotaSnapshot,
    ProviderQuotaSource, ProviderQuotaWindow,
};

pub fn quota_window_from_value(name: &str, value: &Value) -> ProviderQuotaWindow {
    let number = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(Value::as_f64))
            .filter(|value| value.is_finite())
    };
    let string = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_string))
    };
    let limit = number(&["limit"]);
    let remaining = number(&["remaining"]);
    let used = number(&["used"]);
    let used_percent = number(&["used_percent", "percent_used"])
        .map(|percent| percent.clamp(0.0, 100.0))
        .or_else(|| match (limit, remaining) {
            (Some(limit), Some(remaining)) if limit > 0.0 => {
                Some((100.0 * (1.0 - remaining / limit)).clamp(0.0, 100.0))
            }
            _ => None,
        })
        .or_else(|| match (limit, used) {
            (Some(limit), Some(used)) if limit > 0.0 && used >= 0.0 => {
                Some((100.0 * used / limit).clamp(0.0, 100.0))
            }
            _ => None,
        });
    ProviderQuotaWindow {
        id: name.to_string(),
        label: name.replace('_', " "),
        used_percent,
        limit,
        used,
        remaining,
        reset_at: string(&["reset_at", "resets_at", "reset"]),
        reset_after_seconds: value.get("reset_after_seconds").and_then(Value::as_u64),
        window_seconds: ["window_seconds", "limit_window_seconds"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_u64)),
        status: string(&["status"]),
    }
}

pub fn quota_fact(name: &str, value: Option<&Value>) -> Option<ProviderQuotaFact> {
    let value = match value? {
        Value::String(value) => ProviderQuotaFactValue::String(value.clone()),
        Value::Number(value) => ProviderQuotaFactValue::Number(value.clone()),
        Value::Bool(value) => ProviderQuotaFactValue::Boolean(*value),
        Value::Null => ProviderQuotaFactValue::Null,
        Value::Array(_) | Value::Object(_) => return None,
    };
    Some(ProviderQuotaFact {
        id: name.to_string(),
        label: name.replace('_', " "),
        value,
        unit: None,
    })
}

pub fn quota_currency_fact(name: &str, value: Option<&Value>) -> Option<ProviderQuotaFact> {
    let number = value?.as_f64().filter(|value| value.is_finite())?;
    Some(ProviderQuotaFact {
        id: name.to_string(),
        label: name.replace('_', " "),
        value: ProviderQuotaFactValue::Number(serde_json::Number::from_f64(number)?),
        unit: Some("USD".to_string()),
    })
}

pub fn quota_snapshot_from_details(
    provider: &str,
    base_provider: &str,
    source: ProviderQuotaSource,
    details: Value,
) -> ProviderQuotaSnapshot {
    let mut windows = Vec::new();
    let mut facts = Vec::new();
    let available = match source {
        ProviderQuotaSource::XaiOauth => details
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => true,
    };
    let plan = details
        .get("plan_type")
        .or_else(|| details.get("plan"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match source {
        ProviderQuotaSource::ClaudeCode => {
            for key in [
                "five_hour",
                "seven_day",
                "seven_day_sonnet",
                "seven_day_opus",
                "seven_day_oauth_apps",
                "seven_day_cowork",
                "seven_day_omelette",
            ] {
                if let Some(value) = details.get(key).filter(|value| value.is_object()) {
                    windows.push(quota_window_from_value(key, value));
                }
            }
            if let Some(scoped) = details.get("scoped_windows").and_then(Value::as_array) {
                for item in scoped {
                    if let Some(window) = item.get("window") {
                        let name = item
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or("scoped");
                        windows.push(quota_window_from_value(name, window));
                    }
                }
            }
            if let Some(extra) = details.get("extra_usage") {
                facts.extend(
                    [
                        quota_fact("used_credits", extra.get("used_credits")),
                        quota_fact("monthly_limit", extra.get("monthly_limit")),
                        quota_fact("currency", extra.get("currency")),
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
        }
        ProviderQuotaSource::OpenaiCodex => {
            for (prefix, rate_limit) in [
                ("rate_limit", details.get("rate_limit")),
                ("code_review", details.get("code_review_rate_limit")),
            ] {
                if let Some(rate_limit) = rate_limit {
                    for (suffix, window) in [
                        ("primary", rate_limit.get("primary_window")),
                        ("secondary", rate_limit.get("secondary_window")),
                    ] {
                        if let Some(window) = window.filter(|value| value.is_object()) {
                            windows.push(quota_window_from_value(
                                &format!("{prefix}_{suffix}"),
                                window,
                            ));
                        }
                    }
                }
            }
            if let Some(additional) = details
                .get("additional_rate_limits")
                .and_then(Value::as_array)
            {
                for (index, item) in additional.iter().enumerate() {
                    let name = item
                        .get("limit_name")
                        .or_else(|| item.get("metered_feature"))
                        .and_then(Value::as_str)
                        .unwrap_or("additional");
                    if let Some(rate_limit) = item.get("rate_limit") {
                        for (suffix, window) in [
                            ("primary", rate_limit.get("primary_window")),
                            ("secondary", rate_limit.get("secondary_window")),
                        ] {
                            if let Some(window) = window.filter(|value| value.is_object()) {
                                windows.push(quota_window_from_value(
                                    &format!("additional_{index}_{name}_{suffix}"),
                                    window,
                                ));
                            }
                        }
                    }
                }
            }
            if let Some(credits) = details.get("credits") {
                facts.extend(
                    [
                        quota_fact("credit_balance", credits.get("balance")),
                        quota_fact("credits_unlimited", credits.get("unlimited")),
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
            if let Some(reset_credits) = details.get("rate_limit_reset_credits") {
                facts.extend(
                    [quota_fact(
                        "reset_credits_available",
                        reset_credits.get("available_count"),
                    )]
                    .into_iter()
                    .flatten(),
                );
            }
        }
        ProviderQuotaSource::OpenRouter => {
            let window = quota_window_from_value(
                "credits",
                &json!({
                    "limit": details.get("limit"),
                    "used": details.get("usage"),
                    "remaining": details.get("limit_remaining"),
                }),
            );
            if window.limit.is_some() || window.used.is_some() || window.remaining.is_some() {
                windows.push(window);
            }
        }
        ProviderQuotaSource::Opencode => {
            for key in ["rolling", "weekly", "monthly"] {
                if let Some(value) = details.get(key).filter(|value| value.is_object()) {
                    windows.push(quota_window_from_value(key, value));
                }
            }
            facts.extend(
                [quota_fact("balance", details.get("balance"))]
                    .into_iter()
                    .flatten(),
            );
        }
        ProviderQuotaSource::GoogleAntigravity => {
            if let Some(groups) = details.get("groups").and_then(Value::as_array) {
                for group in groups {
                    let group_name = group
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or("quota");
                    if let Some(buckets) = group.get("buckets").and_then(Value::as_array) {
                        for bucket in buckets {
                            let name = bucket
                                .get("display_name")
                                .and_then(Value::as_str)
                                .unwrap_or(group_name);
                            let remaining_fraction = bucket
                                .get("remaining_fraction")
                                .and_then(Value::as_f64)
                                .filter(|value| value.is_finite());
                            let mut window = quota_window_from_value(name, bucket);
                            window.used_percent = remaining_fraction
                                .map(|value| 100.0 * (1.0 - value.clamp(0.0, 1.0)));
                            windows.push(window);
                        }
                    }
                }
            }
        }
        ProviderQuotaSource::XaiOauth => {
            if let Some(values) = details.get("windows").and_then(Value::as_array) {
                for (index, value) in values.iter().enumerate() {
                    let name = value
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("window_{index}"));
                    windows.push(quota_window_from_value(&name, value));
                }
            }
        }
        ProviderQuotaSource::Litellm => {
            facts.extend(
                [
                    quota_currency_fact("spend", details.get("spend")),
                    quota_currency_fact("max_budget", details.get("max_budget")),
                    quota_currency_fact("remaining", details.get("remaining")),
                ]
                .into_iter()
                .flatten(),
            );
        }
        ProviderQuotaSource::Unavailable => {}
    }
    ProviderQuotaSnapshot {
        provider_name: provider.to_string(),
        base_provider: base_provider.to_string(),
        source,
        available,
        error: None,
        plan,
        windows,
        facts,
        fetched_at: ProviderQuotaCache::now_iso_timestamp(),
        stale: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

        #[test]
        fn codex_quota_mapping_is_private_and_normalizes_windows() {
            let details = json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 25.0,
                        "reset_at": "2026-01-01T00:00:00Z",
                        "limit_window_seconds": 18000
                    }
                },
                "credits": {"balance": 12.5, "unlimited": false}
            });
            let snapshot = quota_snapshot_from_details(
                "codex-work",
                "openai_codex",
                ProviderQuotaSource::OpenaiCodex,
                details.clone(),
            );
            assert_eq!(snapshot.plan.as_deref(), Some("pro"));
            assert_eq!(snapshot.windows.len(), 1);
            assert_eq!(snapshot.windows[0].used_percent, Some(25.0));
            assert_eq!(snapshot.facts.len(), 2);
            let serialized = serde_json::to_value(snapshot).unwrap();
            assert!(serialized.get("details").is_none());
            assert!(serialized.get("account_id").is_none());
        }
    
        #[test]
        fn generic_quota_window_derives_used_percent() {
            let window = quota_window_from_value("daily", &json!({"limit": 200, "remaining": 50}));
            assert_eq!(window.used_percent, Some(75.0));
            assert_eq!(window.remaining, Some(50.0));
        }
    
        #[test]
        fn generic_quota_window_derives_exhausted_percentage() {
            let window = quota_window_from_value("credits", &json!({"limit": 20, "remaining": 0}));
            assert_eq!(window.used_percent, Some(100.0));
    
            let over_limit = quota_window_from_value("credits", &json!({"limit": 20, "remaining": -1}));
            assert_eq!(over_limit.used_percent, Some(100.0));
        }
    
        #[test]
        fn litellm_budget_facts_include_usd_units() {
            let snapshot = quota_snapshot_from_details(
                "litellm-work",
                "litellm",
                ProviderQuotaSource::Litellm,
                json!({"spend": 12.5, "max_budget": 20.0, "remaining": 7.5}),
            );
    
            for name in ["spend", "max_budget", "remaining"] {
                let fact = snapshot
                    .facts
                    .iter()
                    .find(|fact| fact.id == name)
                    .unwrap_or_else(|| panic!("missing {name} fact"));
                assert_eq!(fact.unit.as_deref(), Some("USD"));
            }
    
            let malformed = quota_snapshot_from_details(
                "litellm-work",
                "litellm",
                ProviderQuotaSource::Litellm,
                json!({"spend": null, "max_budget": "unknown", "remaining": 7.5}),
            );
            assert_eq!(malformed.facts.len(), 1);
            assert_eq!(malformed.facts[0].id, "remaining");
            assert_eq!(malformed.facts[0].unit.as_deref(), Some("USD"));
        }
    
        #[test]
        fn openrouter_key_quota_normalizes_usage() {
            let snapshot = quota_snapshot_from_details(
                "openrouter-work",
                "openrouter",
                ProviderQuotaSource::OpenRouter,
                json!({
                    "limit": 100.0,
                    "usage": 25.0
                }),
            );
    
            assert_eq!(snapshot.source, ProviderQuotaSource::OpenRouter);
            assert_eq!(snapshot.windows.len(), 1);
            assert_eq!(snapshot.windows[0].id, "credits");
            assert_eq!(snapshot.windows[0].limit, Some(100.0));
            assert_eq!(snapshot.windows[0].used, Some(25.0));
            assert_eq!(snapshot.windows[0].remaining, None);
            assert_eq!(snapshot.windows[0].used_percent, Some(25.0));
            assert_eq!(snapshot.windows[0].reset_at, None);
            assert!(snapshot.facts.is_empty());
    
            let with_remaining = quota_snapshot_from_details(
                "openrouter-work",
                "openrouter",
                ProviderQuotaSource::OpenRouter,
                json!({"limit": 100.0, "limit_remaining": 40.0}),
            );
            assert_eq!(with_remaining.windows[0].remaining, Some(40.0));
            assert_eq!(with_remaining.windows[0].used_percent, Some(60.0));
        }
    
        #[test]
        fn openrouter_key_quota_omits_empty_window_and_clamps_over_limit_usage() {
            let absent = quota_snapshot_from_details(
                "openrouter-work",
                "openrouter",
                ProviderQuotaSource::OpenRouter,
                json!({"limit": null, "usage": "invalid", "limit_remaining": null}),
            );
            assert!(absent.windows.is_empty());
    
            let over_limit = quota_snapshot_from_details(
                "openrouter-work",
                "openrouter",
                ProviderQuotaSource::OpenRouter,
                json!({"limit": 100.0, "usage": 125.0}),
            );
            assert_eq!(over_limit.windows.len(), 1);
            assert_eq!(over_limit.windows[0].used_percent, Some(100.0));
        }
    
        #[test]
        fn antigravity_quota_mapping_normalizes_fraction_without_absolute_remaining() {
            let snapshot = quota_snapshot_from_details(
                "antigravity-work",
                "google_antigravity",
                ProviderQuotaSource::GoogleAntigravity,
                json!({"groups": [{"display_name": "Gemini", "buckets": [{
                    "display_name": "Daily", "remaining_fraction": 0.8, "reset": "tomorrow"
                }]}]}),
            );
            assert_eq!(snapshot.windows[0].remaining, None);
            assert!(snapshot.windows[0]
                .used_percent
                .is_some_and(|value| (value - 20.0).abs() < 1e-9));
            assert_eq!(snapshot.windows[0].reset_at.as_deref(), Some("tomorrow"));
        }
    
        #[test]
        fn claude_quota_includes_all_documented_and_scoped_windows_without_plan() {
            let mut details = serde_json::Map::new();
            for key in [
                "five_hour",
                "seven_day",
                "seven_day_sonnet",
                "seven_day_oauth_apps",
                "seven_day_opus",
                "seven_day_cowork",
                "seven_day_omelette",
            ] {
                details.insert(key.to_string(), json!({"used_percent": 1}));
            }
            details.insert(
                "scoped_windows".to_string(),
                json!([{"label": "Fable", "window": {"used_percent": 2}}]),
            );
            details.insert("email".to_string(), json!("private@example.com"));
            let snapshot = quota_snapshot_from_details(
                "claude-work",
                "claude_code",
                ProviderQuotaSource::ClaudeCode,
                Value::Object(details),
            );
    
            assert!(snapshot.plan.is_none());
            assert_eq!(snapshot.windows.len(), 8);
            assert!(serde_json::to_value(snapshot)
                .unwrap()
                .get("email")
                .is_none());
        }
    
        #[test]
        fn codex_quota_includes_additional_windows_and_reset_credit_fact() {
            let snapshot = quota_snapshot_from_details(
                "codex-work",
                "openai_codex",
                ProviderQuotaSource::OpenaiCodex,
                json!({
                    "rate_limit": {"primary_window": {"used_percent": 10}},
                    "code_review_rate_limit": {"secondary_window": {"used_percent": 20}},
                    "additional_rate_limits": [{
                        "limit_name": "Spark",
                        "rate_limit": {"primary_window": {"used_percent": 30}}
                    }],
                    "rate_limit_reset_credits": {"available_count": 2}
                }),
            );
    
            assert_eq!(snapshot.windows.len(), 3);
            assert!(snapshot
                .facts
                .iter()
                .any(|fact| fact.id == "reset_credits_available"));
        }
}
