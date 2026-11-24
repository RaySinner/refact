use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{GoalAttempt, GoalBudget, GoalCriterion, GoalEvent, GoalProgress};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Active,
    Verifying,
    Paused,
    Completed,
    Stopped,
    BudgetExhausted,
    NoProgress,
    Transferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalResolution {
    #[default]
    Active,
    Achieved,
    Failed,
    Cancelled,
}

impl GoalResolution {
    pub fn from_legacy_status(status: GoalStatus) -> Self {
        match status {
            GoalStatus::Completed => Self::Achieved,
            GoalStatus::Stopped => Self::Cancelled,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalExecution {
    #[default]
    Idle,
    Running,
    Verifying,
    WaitingInput,
    WaitingApproval,
    Paused,
    RetryScheduled,
}

impl GoalExecution {
    pub fn from_legacy_status(status: GoalStatus) -> Self {
        match status {
            GoalStatus::Active => Self::Idle,
            GoalStatus::Verifying => Self::Verifying,
            GoalStatus::Paused
            | GoalStatus::BudgetExhausted
            | GoalStatus::NoProgress
            | GoalStatus::Transferred => Self::Paused,
            GoalStatus::Completed | GoalStatus::Stopped => Self::Idle,
        }
    }

    pub fn allows_scheduling(self) -> bool {
        matches!(self, Self::Idle | Self::RetryScheduled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalAutonomy {
    Off,
    #[default]
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalOwner {
    #[default]
    User,
    Delegate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSnapshot {
    pub content: String,
    pub version: u32,
    pub active: bool,
    pub status: GoalStatus,
    pub resolution: GoalResolution,
    pub execution: GoalExecution,
    pub autonomy: GoalAutonomy,
    pub owner: GoalOwner,
    pub revision: u64,
    pub can_pursue: bool,
    pub prompt_enabled: bool,
    pub budget: GoalBudget,
    pub progress: GoalProgress,
    pub attempts: Vec<GoalAttempt>,
    pub events: Vec<GoalEvent>,
    pub criteria: Vec<GoalCriterion>,
    pub snoozed_until_ms: Option<u64>,
    pub stop_reason: Option<String>,
    pub transferred_from: Option<String>,
    pub transferred_to: Option<String>,
}

impl Default for GoalSnapshot {
    fn default() -> Self {
        Self {
            content: String::new(),
            version: 0,
            active: false,
            status: GoalStatus::Active,
            resolution: GoalResolution::Active,
            execution: GoalExecution::Idle,
            autonomy: GoalAutonomy::On,
            owner: GoalOwner::User,
            revision: 1,
            can_pursue: false,
            prompt_enabled: false,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        }
    }
}

impl GoalSnapshot {
    pub fn apply_legacy_status(&mut self, status: GoalStatus) {
        let previous = self.status;
        self.status = status;
        if status == GoalStatus::Active
            && previous == GoalStatus::Stopped
            && self.resolution == GoalResolution::Cancelled
        {
            self.resolution = GoalResolution::Active;
        }
        apply_legacy_status_dimensions(&mut self.resolution, &mut self.execution, status);
        self.refresh_derived();
    }

    pub fn goal_can_pursue_at(&self, now_ms: u64) -> bool {
        self.active
            && self.resolution == GoalResolution::Active
            && self.autonomy == GoalAutonomy::On
            && self.execution.allows_scheduling()
            && self.transferred_to.is_none()
            && crate::goal_budget_exhaustion_status(&self.budget, &self.progress, now_ms).is_none()
    }

    pub fn goal_prompt_enabled(&self) -> bool {
        self.active
            && self.resolution == GoalResolution::Active
            && self.execution != GoalExecution::Paused
            && self.transferred_to.is_none()
    }

    pub fn refresh_derived(&mut self) {
        self.can_pursue = self.goal_can_pursue_at(epoch_ms_now());
        self.prompt_enabled = self.goal_prompt_enabled();
    }
}

pub fn apply_legacy_status_dimensions(
    resolution: &mut GoalResolution,
    execution: &mut GoalExecution,
    status: GoalStatus,
) {
    match status {
        GoalStatus::Active => {
            if *resolution == GoalResolution::Active {
                *execution = GoalExecution::Idle;
            }
        }
        GoalStatus::Verifying => {
            if *resolution == GoalResolution::Active {
                *execution = GoalExecution::Verifying;
            }
        }
        GoalStatus::Paused => {
            *execution = GoalExecution::Paused;
        }
        GoalStatus::Completed => {
            *resolution = GoalResolution::Achieved;
            *execution = GoalExecution::Idle;
        }
        GoalStatus::Stopped => {
            if *resolution == GoalResolution::Active {
                *resolution = GoalResolution::Cancelled;
            }
            *execution = GoalExecution::Idle;
        }
        GoalStatus::BudgetExhausted | GoalStatus::NoProgress => {
            if *resolution == GoalResolution::Active {
                *execution = GoalExecution::Paused;
            }
        }
        GoalStatus::Transferred => {
            *execution = GoalExecution::Paused;
        }
    }
}

fn epoch_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GoalSnapshotWire {
    content: String,
    version: u32,
    active: bool,
    status: GoalStatus,
    #[serde(default)]
    resolution: Option<GoalResolution>,
    #[serde(default)]
    execution: Option<GoalExecution>,
    #[serde(default)]
    autonomy: Option<GoalAutonomy>,
    #[serde(default)]
    owner: Option<GoalOwner>,
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    can_pursue: Option<bool>,
    #[serde(default)]
    prompt_enabled: Option<bool>,
    budget: GoalBudget,
    progress: GoalProgress,
    attempts: Vec<GoalAttempt>,
    events: Vec<GoalEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    criteria: Vec<GoalCriterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snoozed_until_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
    transferred_from: Option<String>,
    transferred_to: Option<String>,
}

impl From<GoalSnapshot> for GoalSnapshotWire {
    fn from(mut goal: GoalSnapshot) -> Self {
        goal.refresh_derived();
        Self {
            content: goal.content,
            version: goal.version,
            active: goal.active,
            status: goal.status,
            resolution: Some(goal.resolution),
            execution: Some(goal.execution),
            autonomy: Some(goal.autonomy),
            owner: Some(goal.owner),
            revision: Some(goal.revision),
            can_pursue: Some(goal.can_pursue),
            prompt_enabled: Some(goal.prompt_enabled),
            budget: goal.budget,
            progress: goal.progress,
            attempts: goal.attempts,
            events: goal.events,
            criteria: goal.criteria,
            snoozed_until_ms: goal.snoozed_until_ms,
            stop_reason: goal.stop_reason,
            transferred_from: goal.transferred_from,
            transferred_to: goal.transferred_to,
        }
    }
}

impl Serialize for GoalSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        GoalSnapshotWire::from(self.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GoalSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GoalSnapshotWire::deserialize(deserializer)?;
        let resolution = wire
            .resolution
            .unwrap_or_else(|| GoalResolution::from_legacy_status(wire.status));
        let execution = wire
            .execution
            .unwrap_or_else(|| GoalExecution::from_legacy_status(wire.status));
        let _ = (wire.can_pursue, wire.prompt_enabled);
        let mut goal = Self {
            content: wire.content,
            version: wire.version,
            active: wire.active,
            status: wire.status,
            resolution,
            execution,
            autonomy: wire.autonomy.unwrap_or_default(),
            owner: wire.owner.unwrap_or_default(),
            revision: wire.revision.unwrap_or(1),
            can_pursue: false,
            prompt_enabled: false,
            budget: wire.budget,
            progress: wire.progress,
            attempts: wire.attempts,
            events: wire.events,
            criteria: wire.criteria,
            snoozed_until_ms: wire.snoozed_until_ms,
            stop_reason: wire.stop_reason,
            transferred_from: wire.transferred_from,
            transferred_to: wire.transferred_to,
        };
        goal.refresh_derived();
        Ok(goal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn legacy_json(status: GoalStatus) -> serde_json::Value {
        json!({
            "content": "ship it",
            "version": 2,
            "active": true,
            "status": status,
            "budget": GoalBudget::default(),
            "progress": GoalProgress::default(),
            "attempts": [],
            "events": [],
            "transferred_from": null,
            "transferred_to": null
        })
    }

    #[test]
    fn goal_legacy_statuses_map_without_losing_wire_status() {
        for (status, resolution, execution) in [
            (
                GoalStatus::Active,
                GoalResolution::Active,
                GoalExecution::Idle,
            ),
            (
                GoalStatus::Verifying,
                GoalResolution::Active,
                GoalExecution::Verifying,
            ),
            (
                GoalStatus::Paused,
                GoalResolution::Active,
                GoalExecution::Paused,
            ),
            (
                GoalStatus::Completed,
                GoalResolution::Achieved,
                GoalExecution::Idle,
            ),
            (
                GoalStatus::Stopped,
                GoalResolution::Cancelled,
                GoalExecution::Idle,
            ),
            (
                GoalStatus::BudgetExhausted,
                GoalResolution::Active,
                GoalExecution::Paused,
            ),
            (
                GoalStatus::NoProgress,
                GoalResolution::Active,
                GoalExecution::Paused,
            ),
            (
                GoalStatus::Transferred,
                GoalResolution::Active,
                GoalExecution::Paused,
            ),
        ] {
            let goal: GoalSnapshot = serde_json::from_value(legacy_json(status)).unwrap();
            assert_eq!(goal.status, status);
            assert_eq!(goal.resolution, resolution);
            assert_eq!(goal.execution, execution);
            assert_eq!(goal.autonomy, GoalAutonomy::On);
            assert_eq!(goal.owner, GoalOwner::User);
            assert_eq!(goal.revision, 1);
        }
    }

    #[test]
    fn goal_can_pursue_requires_each_independent_gate() {
        let mut goal = GoalSnapshot {
            active: true,
            ..Default::default()
        };
        goal.refresh_derived();
        assert!(goal.can_pursue);

        for execution in [
            GoalExecution::Running,
            GoalExecution::Verifying,
            GoalExecution::WaitingInput,
            GoalExecution::WaitingApproval,
            GoalExecution::Paused,
        ] {
            goal.execution = execution;
            goal.refresh_derived();
            assert!(!goal.can_pursue, "{execution:?}");
            assert_eq!(goal.prompt_enabled, execution != GoalExecution::Paused);
        }

        goal.execution = GoalExecution::RetryScheduled;
        goal.refresh_derived();
        assert!(goal.can_pursue);
        goal.autonomy = GoalAutonomy::Off;
        goal.refresh_derived();
        assert!(!goal.can_pursue);
        assert!(goal.prompt_enabled);
        goal.autonomy = GoalAutonomy::On;
        goal.budget.max_turns = Some(1);
        goal.progress.turns_used = 1;
        goal.refresh_derived();
        assert!(!goal.can_pursue);
        assert!(goal.prompt_enabled);
        goal.budget.max_turns = None;
        goal.resolution = GoalResolution::Failed;
        goal.refresh_derived();
        assert!(!goal.can_pursue);
        assert!(!goal.prompt_enabled);
    }

    #[test]
    fn goal_transfer_ownership_controls_pursuit() {
        let mut goal = GoalSnapshot {
            active: true,
            ..Default::default()
        };
        goal.refresh_derived();
        assert!(goal.can_pursue);
        assert!(goal.prompt_enabled);

        goal.transferred_from = Some("source-chat".to_string());
        goal.refresh_derived();
        assert!(goal.can_pursue);
        assert!(goal.prompt_enabled);

        goal.transferred_to = Some("target-chat".to_string());
        goal.refresh_derived();
        assert!(!goal.goal_can_pursue_at(0));
        assert!(!goal.can_pursue);
        assert!(!goal.prompt_enabled);
    }

    #[test]
    fn goal_derived_fields_are_recomputed_during_serde() {
        let goal = GoalSnapshot {
            content: "ship".to_string(),
            version: 1,
            active: true,
            owner: GoalOwner::Delegate,
            revision: 4,
            autonomy: GoalAutonomy::Off,
            can_pursue: true,
            prompt_enabled: false,
            ..Default::default()
        };
        let value = serde_json::to_value(&goal).unwrap();
        assert_eq!(value["can_pursue"], json!(false));
        assert_eq!(value["prompt_enabled"], json!(true));
        assert_eq!(value["owner"], json!("delegate"));
        assert_eq!(value["revision"], json!(4));

        let decoded: GoalSnapshot = serde_json::from_value(value).unwrap();
        assert!(!decoded.can_pursue);
        assert!(decoded.prompt_enabled);
        assert_eq!(decoded.owner, GoalOwner::Delegate);
        assert_eq!(decoded.revision, 4);
    }

    #[test]
    fn goal_terminal_resolution_is_not_reanimated_by_legacy_metadata() {
        let mut value = legacy_json(GoalStatus::Active);
        value["resolution"] = json!("achieved");
        let goal: GoalSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(goal.resolution, GoalResolution::Achieved);
        assert!(!goal.can_pursue);
        assert!(!goal.prompt_enabled);
    }

    #[test]
    fn goal_legacy_manual_stop_can_resume_but_completed_cannot() {
        let mut stopped = GoalSnapshot {
            active: true,
            ..Default::default()
        };
        stopped.apply_legacy_status(GoalStatus::Stopped);
        stopped.apply_legacy_status(GoalStatus::Active);
        assert_eq!(stopped.resolution, GoalResolution::Active);
        assert!(stopped.can_pursue);

        let mut completed = GoalSnapshot {
            active: true,
            ..Default::default()
        };
        completed.apply_legacy_status(GoalStatus::Completed);
        completed.apply_legacy_status(GoalStatus::Stopped);
        completed.apply_legacy_status(GoalStatus::Active);
        assert_eq!(completed.resolution, GoalResolution::Achieved);
        assert!(!completed.can_pursue);
        assert!(!completed.prompt_enabled);
    }
}
