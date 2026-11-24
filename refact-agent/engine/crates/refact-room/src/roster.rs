use crate::RoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RosterMember<'a> {
    pub role: &'a str,
    pub writes: bool,
    pub claimed: bool,
}

pub fn validate_roster(members: &[RosterMember<'_>]) -> Result<(), RoomError> {
    validate_writers(members, true)
}

pub(crate) fn validate_complete_roster(members: &[RosterMember<'_>]) -> Result<(), RoomError> {
    validate_writers(members, false)
}

fn validate_writers(members: &[RosterMember<'_>], legacy_single: bool) -> Result<(), RoomError> {
    if members.is_empty() || (legacy_single && members.len() == 1) {
        return Ok(());
    }
    let writers = members
        .iter()
        .filter(|member| member.writes)
        .collect::<Vec<_>>();
    if writers.len() == 1 {
        return Ok(());
    }
    let roles = writers
        .iter()
        .map(|member| member.role)
        .collect::<Vec<_>>()
        .join(", ");
    Err(RoomError::Validation(format!(
        "multi-member room requires exactly one writer (found {}{}); set writes=true on exactly one agent definition and writes=false on every advisor",
        writers.len(),
        if roles.is_empty() {
            String::new()
        } else {
            format!(": {roles}")
        }
    )))
}

pub fn validate_spawn(members: &[RosterMember<'_>], role: &str) -> Result<(), RoomError> {
    validate_roster(members)?;
    if members.len() <= 1 {
        return Ok(());
    }
    let writer = members
        .iter()
        .find(|member| member.writes)
        .expect("validated roster has one writer");
    if !writer.claimed && writer.role.trim() != role.trim() {
        return Err(RoomError::Validation(format!(
            "writer role '{}' must be spawned before advisor '{}'; spawn the writer first",
            writer.role,
            role.trim()
        )));
    }
    Ok(())
}

pub fn spawn_order(members: &[RosterMember<'_>]) -> Result<Vec<usize>, RoomError> {
    validate_roster(members)?;
    let writer = members.iter().position(|member| member.writes);
    Ok((0..members.len())
        .filter(|index| Some(*index) != writer)
        .fold(
            writer.into_iter().collect::<Vec<_>>(),
            |mut order, index| {
                order.push(index);
                order
            },
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(role: &str, writes: bool, claimed: bool) -> RosterMember<'_> {
        RosterMember {
            role,
            writes,
            claimed,
        }
    }

    #[test]
    fn multi_member_roster_requires_exactly_one_writer() {
        let advisor = member("reviewer", false, false);
        let writer = member("coder", true, false);
        let other_writer = member("designer", true, false);

        assert!(validate_roster(&[advisor, writer]).is_ok());
        assert!(validate_roster(&[advisor, advisor]).is_err());
        assert!(validate_roster(&[writer, other_writer]).is_err());
    }

    #[test]
    fn single_member_legacy_roster_does_not_gain_a_writer_requirement() {
        assert!(validate_roster(&[member("reviewer", false, false)]).is_ok());
        assert!(validate_spawn(&[member("reviewer", false, false)], "reviewer").is_ok());
    }

    #[test]
    fn writer_must_be_claimed_before_advisors() {
        let roster = [
            member("reviewer", false, false),
            member("coder", true, false),
        ];
        let error = validate_spawn(&roster, "reviewer").unwrap_err().to_string();
        assert!(error.contains("coder"), "{error}");
        assert!(error.contains("reviewer"), "{error}");
        assert!(validate_spawn(&roster, "coder").is_ok());

        let started = [
            member("reviewer", false, false),
            member("coder", true, true),
        ];
        assert!(validate_spawn(&started, "reviewer").is_ok());
    }

    #[test]
    fn launch_order_is_writer_first_then_roster_order() {
        let roster = [
            member("reviewer", false, false),
            member("coder", true, false),
            member("qa", false, false),
        ];
        assert_eq!(spawn_order(&roster).unwrap(), vec![1, 0, 2]);
        assert!(spawn_order(&[roster[0], roster[2]]).is_err());
    }
}
