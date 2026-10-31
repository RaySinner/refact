use std::path::Path;
use std::sync::Arc;

use refact_core::privacy_types::{FilePrivacyLevel, FilePrivacySettings, PrivacySettings};

use crate::PrivacyPolicy;

pub fn legacy_settings(policy: &PrivacyPolicy, loaded_ts: u64) -> PrivacySettings {
    let mut blocked = policy.blocked.clone();
    let mut only_send_to_servers_i_control = Vec::new();
    for zone in &policy.zones {
        if zone.send_to.is_empty() {
            blocked.extend(zone.patterns.iter().cloned());
        } else if zone.send_to.as_slice() != ["*"] {
            only_send_to_servers_i_control.extend(zone.patterns.iter().cloned());
        }
    }
    PrivacySettings {
        privacy_rules: FilePrivacySettings {
            blocked,
            only_send_to_servers_I_control: only_send_to_servers_i_control,
        },
        loaded_ts,
    }
}

fn legacy_patterns(patterns: &[String]) -> Vec<String> {
    let mut expanded = Vec::with_capacity(patterns.len() * 2);
    for pattern in patterns {
        expanded.push(pattern.clone());
        if !pattern
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']'))
        {
            expanded.push(format!("*{pattern}"));
        }
    }
    expanded
}

fn legacy_policy(privacy_settings: &PrivacySettings) -> PrivacyPolicy {
    let mut policy = crate::migrate_legacy(crate::LegacyPrivacyPolicy {
        blocked: legacy_patterns(&privacy_settings.privacy_rules.blocked),
        only_send_to_servers_i_control: legacy_patterns(
            &privacy_settings
                .privacy_rules
                .only_send_to_servers_I_control,
        ),
    });
    if let Some(zone) = policy
        .zones
        .iter_mut()
        .find(|zone| zone.name == "only_send_to_servers_i_control")
    {
        zone.send_to.push("legacy_controlled_server".to_string());
    }
    policy
}

fn get_file_privacy_level(privacy_settings: Arc<PrivacySettings>, path: &Path) -> FilePrivacyLevel {
    let policy = match legacy_policy(&privacy_settings).compile() {
        Ok(policy) => policy,
        Err(_) => return FilePrivacyLevel::Blocked,
    };
    let zone = policy.zone_for_path(path);
    if zone.name == "blocked" {
        FilePrivacyLevel::Blocked
    } else if zone.send_to.len() == 1 && zone.send_to[0] == "*" {
        FilePrivacyLevel::AllowToSendAnywhere
    } else if !zone.send_to.is_empty() {
        FilePrivacyLevel::OnlySendToServersIControl
    } else {
        FilePrivacyLevel::Blocked
    }
}

pub fn check_file_privacy(
    privacy_settings: Arc<PrivacySettings>,
    path: &Path,
    min_allowed_privacy_level: &FilePrivacyLevel,
) -> Result<(), String> {
    let file_privacy_level = get_file_privacy_level(privacy_settings.clone(), path);
    if file_privacy_level < *min_allowed_privacy_level {
        return Err(format!("privacy level {:?}", file_privacy_level));
    }
    Ok(())
}

pub fn check_file_privacy_for_read_preparation(
    privacy_settings: Arc<PrivacySettings>,
    path: &Path,
) -> Result<(), String> {
    let file_privacy_level = get_file_privacy_level(privacy_settings, path);
    if file_privacy_level == FilePrivacyLevel::Blocked {
        return Err(format!("privacy level {:?}", file_privacy_level));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, sync::Arc};

    #[test]
    fn test_privacy_patterns() {
        let privacy_settings = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: vec![
                    "*.pem".to_string(),
                    "*/semi_private_dir/*.md".to_string(),
                ],
                blocked: vec![
                    "*.pem".to_string(),
                    "*/secret_dir/*".to_string(),
                    "secret_passwords.txt".to_string(),
                ],
            },
            loaded_ts: 0,
        });

        let current_dir = std::env::current_dir().unwrap();

        let cases: Vec<(PathBuf, FilePrivacyLevel)> = vec![
            (current_dir.join("secret.pem"), FilePrivacyLevel::Blocked),
            (
                current_dir.join("somedir/secret.pem"),
                FilePrivacyLevel::Blocked,
            ),
            (
                current_dir.join("secret.pub"),
                FilePrivacyLevel::AllowToSendAnywhere,
            ),
            (
                current_dir.join("secret_passwords.txt"),
                FilePrivacyLevel::Blocked,
            ),
            (
                current_dir.join("3/2/1/secret_passwords.txt"),
                FilePrivacyLevel::Blocked,
            ),
            (
                current_dir.join("secret_passwords.jpeg"),
                FilePrivacyLevel::AllowToSendAnywhere,
            ),
            (
                current_dir.join("secret_dir/anything.jpg"),
                FilePrivacyLevel::Blocked,
            ),
            (
                current_dir.join("semi_private_dir/wow1.md"),
                FilePrivacyLevel::OnlySendToServersIControl,
            ),
            (
                current_dir.join("semi_private_dir/wow1.jpeg"),
                FilePrivacyLevel::AllowToSendAnywhere,
            ),
            (
                current_dir.join("1/2/3/semi_private_dir/wow1.md"),
                FilePrivacyLevel::OnlySendToServersIControl,
            ),
            (
                current_dir.join("1/2/3/semi_private_dir/4/5/6/wow1.md"),
                FilePrivacyLevel::OnlySendToServersIControl,
            ),
            (
                current_dir.join("wow1.md"),
                FilePrivacyLevel::AllowToSendAnywhere,
            ),
        ];

        for (path, expected_privacy_level) in cases {
            let actual_privacy_level = get_file_privacy_level(privacy_settings.clone(), &path);
            assert_eq!(
                actual_privacy_level,
                expected_privacy_level,
                "Testing get_file_privacy_level with path {} and expected privacy level {:?}, got {:?}",
                path.display(),
                expected_privacy_level,
                actual_privacy_level,
            );
        }
    }

    #[test]
    fn test_privacy_minimum() {
        let privacy_settings = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: vec![
                    "*.cat.txt".to_string(),
                    "*.md".to_string(),
                    "*/.venv/*".to_string(),
                    "**/tests_dir/**/*".to_string(),
                ],
                blocked: vec!["*/make.png".to_string(), "*.txt".to_string()],
            },
            loaded_ts: 0,
        });

        let current_dir = std::env::current_dir().unwrap();

        let cases: Vec<(PathBuf, FilePrivacyLevel, bool)> = vec![
            (
                current_dir.join("test.zip"),
                FilePrivacyLevel::AllowToSendAnywhere,
                true,
            ),
            (
                current_dir.join("test.md"),
                FilePrivacyLevel::AllowToSendAnywhere,
                false,
            ),
            (
                current_dir.join("test.md"),
                FilePrivacyLevel::OnlySendToServersIControl,
                true,
            ),
            (
                current_dir.join("test.cat.txt"),
                FilePrivacyLevel::OnlySendToServersIControl,
                false,
            ),
        ];

        for (path, expected_privacy_level, expected_result) in &cases {
            let result = check_file_privacy(privacy_settings.clone(), path, expected_privacy_level);
            if *expected_result {
                assert!(
                    result.is_ok(),
                    "Testing check_file_privacy with path {} and expected privacy level {:?}, got {:?} and it should have been ok",
                    path.display(),
                    expected_privacy_level,
                    result.unwrap_err(),
                );
            } else {
                assert!(
                    result.is_err(),
                    "Testing check_file_privacy with path {} and expected privacy level {:?}, got {:?} and it should have been err",
                    path.display(),
                    expected_privacy_level,
                    result.unwrap(),
                );
            }
        }
    }

    #[test]
    fn read_preparation_allows_controlled_but_legacy_check_stays_all_or_nothing() {
        let privacy_settings = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: vec!["*.controlled".to_string()],
                blocked: vec!["*.blocked".to_string()],
            },
            loaded_ts: 0,
        });
        let controlled = PathBuf::from("context.controlled");
        let blocked = PathBuf::from("context.blocked");

        assert!(
            check_file_privacy_for_read_preparation(privacy_settings.clone(), &controlled,).is_ok()
        );
        assert!(
            check_file_privacy_for_read_preparation(privacy_settings.clone(), &blocked,).is_err()
        );

        assert!(check_file_privacy(
            privacy_settings.clone(),
            &controlled,
            &FilePrivacyLevel::AllowToSendAnywhere,
        )
        .is_err());
        assert!(check_file_privacy(
            privacy_settings,
            &controlled,
            &FilePrivacyLevel::OnlySendToServersIControl,
        )
        .is_ok());
    }
}
