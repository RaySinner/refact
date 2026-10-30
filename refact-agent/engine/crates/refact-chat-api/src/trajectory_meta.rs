use crate::TaskMeta;

pub const TRAJECTORY_META_TITLE_MAX_CHARS: usize = 120;

pub fn truncate_text_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

pub fn trajectory_meta_title(title: &str) -> String {
    let cleaned = title.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_text_chars(&cleaned, TRAJECTORY_META_TITLE_MAX_CHARS)
}

pub fn task_context_from_task_meta(
    task_meta: Option<&TaskMeta>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    task_meta
        .map(|meta| {
            (
                Some(meta.task_id.clone()),
                Some(meta.role.clone()),
                meta.agent_id.clone(),
                meta.card_id.clone(),
            )
        })
        .unwrap_or((None, None, None, None))
}
