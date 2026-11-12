use crate::retry::classify;

pub struct CommandRunResult {
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

pub fn command_error_summary(result: &CommandRunResult) -> String {
    let stderr = result.stderr.trim();
    if stderr.is_empty() {
        return "Scheduled command failed".to_string();
    }
    format!("Scheduled command failed: {}", first_line(stderr))
}

fn first_line(value: &str) -> &str {
    value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(value)
}

pub fn command_record_error(result: &CommandRunResult) -> String {
    command_error_summary(result)
}

pub fn command_failure_can_retry(result: &CommandRunResult) -> bool {
    classify(&command_retry_text(result)).is_some()
}

pub fn command_retry_text(result: &CommandRunResult) -> String {
    let mut text = String::new();
    if result.stderr.to_ascii_lowercase().contains("timed out") {
        text.push_str("timeout\n");
    }
    text.push_str(&result.stderr);
    if !result.stdout.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&result.stdout);
    }
    if text.trim().is_empty() {
        command_error_summary(result)
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_timeout_failure_is_retryable() {
        let result = CommandRunResult {
            status: "error".to_string(),
            stdout: String::new(),
            stderr: "command timed out after 1 seconds".to_string(),
            exit_code: None,
        };

        assert!(command_failure_can_retry(&result));
    }

    #[test]
    fn command_error_summary_prefers_stderr() {
        let result = CommandRunResult {
            status: "error".to_string(),
            stdout: String::new(),
            stderr: "boom\nmore".to_string(),
            exit_code: Some(2),
        };

        assert_eq!(
            command_error_summary(&result),
            "Scheduled command failed: boom"
        );
    }
}
