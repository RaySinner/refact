use super::*;

pub fn trajectory_index_coordinator_rollout_enabled() -> bool {
    trajectory_index_coordinator_rollout_enabled_for(
        std::env::var(TRAJECTORY_INDEX_COORDINATOR_ENV)
            .ok()
            .as_deref()
            .or_else(|| {
                trajectory_settings::current()
                    .trajectory_index_coordinator_enabled
                    .then_some("1")
            }),
    )
}

pub fn trajectory_index_coordinator_rollout_enabled_for(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1"
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("on")
    })
}
