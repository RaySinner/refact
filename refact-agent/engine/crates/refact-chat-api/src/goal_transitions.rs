use serde::{Deserialize, Serialize};

use crate::{GoalExecution, GoalResolution, GoalSnapshot, GoalStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalActor {
    User,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalTransition {
    Install,
    Replace,
    UpdateRequirements,
    AddProgress,
    Pause,
    Resume,
    Stop,
    Clear,
    SetAutonomy,
    SetBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalTransitionAuthorization {
    pub actor: GoalActor,
    pub transition: GoalTransition,
    pub revision: u64,
}

pub fn authorize_goal_transition(
    goal: Option<&GoalSnapshot>,
    actor: GoalActor,
    transition: GoalTransition,
) -> Result<GoalTransitionAuthorization, String> {
    if transition == GoalTransition::Install {
        if let Some(goal) = goal {
            if goal.transferred_to.is_some() {
                return Err("this chat no longer owns the transferred goal".to_string());
            }
            if goal.active {
                return Err("goal already exists; use update_goal".to_string());
            }
        }
        return Ok(GoalTransitionAuthorization {
            actor,
            transition,
            revision: goal.map_or(1, |goal| goal.revision.saturating_add(1)),
        });
    }

    let goal = goal.ok_or_else(|| "no goal installed; call set_goal first".to_string())?;
    if goal.transferred_to.is_some() || (!goal.active && transition != GoalTransition::Install) {
        return Err("this chat does not own the goal".to_string());
    }
    if actor == GoalActor::Model
        && !matches!(
            transition,
            GoalTransition::AddProgress | GoalTransition::Pause
        )
    {
        return Err(format!(
            "model tools cannot perform {:?}; explicit user command required",
            transition
        ));
    }
    if goal.resolution != GoalResolution::Active
        && !matches!(transition, GoalTransition::Replace | GoalTransition::Clear)
    {
        if !(transition == GoalTransition::Resume
            && goal.resolution == GoalResolution::Cancelled
            && goal.status == GoalStatus::Stopped)
        {
            return Err(
                "terminal goal cannot be changed; replace or clear it explicitly".to_string(),
            );
        }
    }

    match transition {
        GoalTransition::Pause
            if goal.resolution != GoalResolution::Active
                || goal.execution == GoalExecution::Paused
                || !matches!(goal.status, GoalStatus::Active | GoalStatus::Verifying) =>
        {
            return Err("goal pause requires an active, executing goal".to_string());
        }
        GoalTransition::Resume
            if !matches!(goal.status, GoalStatus::Paused | GoalStatus::Stopped) =>
        {
            return Err("goal_control resume requires a paused or stopped goal".to_string());
        }
        GoalTransition::Stop
            if goal.resolution != GoalResolution::Active
                || matches!(goal.status, GoalStatus::Stopped | GoalStatus::Transferred) =>
        {
            return Err("goal stop requires a non-terminal active goal".to_string());
        }
        GoalTransition::UpdateRequirements if goal.resolution != GoalResolution::Active => {
            return Err(
                "terminal goal requirements cannot be updated; replace the goal".to_string(),
            );
        }
        GoalTransition::AddProgress if goal.resolution != GoalResolution::Active => {
            return Err("terminal goal cannot accept progress notes".to_string());
        }
        _ => {}
    }

    let revision = if matches!(
        transition,
        GoalTransition::Replace
            | GoalTransition::UpdateRequirements
            | GoalTransition::Clear
            | GoalTransition::SetAutonomy
            | GoalTransition::SetBudget
    ) {
        goal.revision.saturating_add(1)
    } else {
        goal.revision
    };
    Ok(GoalTransitionAuthorization {
        actor,
        transition,
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GoalAutonomy;

    fn goal() -> GoalSnapshot {
        GoalSnapshot {
            active: true,
            autonomy: GoalAutonomy::On,
            ..Default::default()
        }
    }

    #[test]
    fn goal_transition_matrix_separates_user_and_model_authority() {
        let current = goal();
        for transition in [
            GoalTransition::Replace,
            GoalTransition::UpdateRequirements,
            GoalTransition::Clear,
            GoalTransition::SetAutonomy,
            GoalTransition::SetBudget,
            GoalTransition::Stop,
            GoalTransition::Resume,
        ] {
            assert!(
                authorize_goal_transition(Some(&current), GoalActor::Model, transition).is_err(),
                "{transition:?}"
            );
        }
        assert!(authorize_goal_transition(
            Some(&current),
            GoalActor::Model,
            GoalTransition::AddProgress
        )
        .is_ok());
        assert!(
            authorize_goal_transition(Some(&current), GoalActor::Model, GoalTransition::Pause)
                .is_ok()
        );
        assert!(authorize_goal_transition(
            Some(&current),
            GoalActor::User,
            GoalTransition::UpdateRequirements
        )
        .is_ok());
        assert!(authorize_goal_transition(
            Some(&current),
            GoalActor::User,
            GoalTransition::SetAutonomy
        )
        .is_ok());
    }

    #[test]
    fn goal_transition_matrix_rejects_terminal_reanimation() {
        let mut completed = goal();
        completed.status = GoalStatus::Completed;
        completed.resolution = GoalResolution::Achieved;
        for actor in [GoalActor::User, GoalActor::Model] {
            for transition in [
                GoalTransition::Stop,
                GoalTransition::Pause,
                GoalTransition::Resume,
                GoalTransition::AddProgress,
                GoalTransition::UpdateRequirements,
            ] {
                assert!(
                    authorize_goal_transition(Some(&completed), actor, transition).is_err(),
                    "{actor:?} {transition:?}"
                );
            }
        }
        assert!(authorize_goal_transition(
            Some(&completed),
            GoalActor::User,
            GoalTransition::Replace
        )
        .is_ok());
        assert!(authorize_goal_transition(
            Some(&completed),
            GoalActor::User,
            GoalTransition::Clear
        )
        .is_ok());
    }

    #[test]
    fn goal_transition_matrix_allows_manual_stop_resume_but_not_clear_resume() {
        let mut stopped = goal();
        stopped.status = GoalStatus::Stopped;
        stopped.resolution = GoalResolution::Cancelled;
        assert!(
            authorize_goal_transition(Some(&stopped), GoalActor::User, GoalTransition::Resume)
                .is_ok()
        );

        stopped.active = false;
        assert!(
            authorize_goal_transition(Some(&stopped), GoalActor::User, GoalTransition::Resume)
                .is_err()
        );
        assert!(authorize_goal_transition(
            Some(&stopped),
            GoalActor::User,
            GoalTransition::Install
        )
        .is_ok());
    }

    #[test]
    fn goal_transition_matrix_rejects_transferred_owner() {
        let mut transferred = goal();
        transferred.active = false;
        transferred.status = GoalStatus::Transferred;
        transferred.transferred_to = Some("new-owner".to_string());
        for actor in [GoalActor::User, GoalActor::Model] {
            for transition in [
                GoalTransition::Install,
                GoalTransition::Replace,
                GoalTransition::UpdateRequirements,
                GoalTransition::AddProgress,
                GoalTransition::Pause,
                GoalTransition::Resume,
                GoalTransition::Stop,
                GoalTransition::Clear,
                GoalTransition::SetAutonomy,
                GoalTransition::SetBudget,
            ] {
                assert!(
                    authorize_goal_transition(Some(&transferred), actor, transition).is_err(),
                    "{actor:?} {transition:?}"
                );
            }
        }
    }

    #[test]
    fn goal_transition_revision_only_changes_for_requirements_and_configuration() {
        let current = goal();
        for transition in [
            GoalTransition::Replace,
            GoalTransition::UpdateRequirements,
            GoalTransition::Clear,
            GoalTransition::SetAutonomy,
            GoalTransition::SetBudget,
        ] {
            assert_eq!(
                authorize_goal_transition(Some(&current), GoalActor::User, transition)
                    .unwrap()
                    .revision,
                current.revision + 1
            );
        }
        for (actor, transition) in [
            (GoalActor::Model, GoalTransition::AddProgress),
            (GoalActor::Model, GoalTransition::Pause),
            (GoalActor::User, GoalTransition::Stop),
        ] {
            assert_eq!(
                authorize_goal_transition(Some(&current), actor, transition)
                    .unwrap()
                    .revision,
                current.revision
            );
        }
    }
}
