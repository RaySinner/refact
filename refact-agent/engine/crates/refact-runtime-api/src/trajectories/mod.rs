pub mod load;
pub mod model;
pub mod paths;
pub mod queries;
pub mod reload;
pub mod store;
pub mod watcher;
pub mod writer;

pub(crate) use queries::{
    calculate_line_changes_from_chat_messages, calculate_task_progress_from_chat_messages,
    calculate_token_totals_from_chat_messages, canonical_child_path_under_root,
    count_user_messages, ensure_real_dir_tree, is_placeholder_title, is_real_dir, is_real_file,
    safe_new_trajectory_file_in_dir, safe_trajectory_file_in_dir, spawn_task_name_generation_task,
    spawn_title_generation_task, trajectory_meta_title,
};
