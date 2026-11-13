pub fn human_readable_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes < KB {
        format!("{}B", bytes)
    } else if bytes < MB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    }
}

pub fn first_n_chars(msg: &String, n: usize) -> String {
    let mut last_n_chars: String = msg.chars().take(n).collect();
    if last_n_chars.len() == n {
        last_n_chars.push_str("...");
    }
    return last_n_chars.replace("\n", "\\n");
}

pub fn last_n_chars(msg: &String, n: usize) -> String {
    let mut last_n_chars: String = msg
        .chars()
        .rev()
        .take(n)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if last_n_chars.len() == n {
        last_n_chars.insert_str(0, "...");
    }
    return last_n_chars.replace("\n", "\\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_below_one_kilobyte_use_byte_suffix() {
        assert_eq!(human_readable_bytes(0), "0B");
        assert_eq!(human_readable_bytes(1023), "1023B");
    }

    #[test]
    fn larger_values_use_scaled_suffixes() {
        assert_eq!(human_readable_bytes(1024), "1.0KB");
        assert_eq!(human_readable_bytes(1536), "1.5KB");
        assert_eq!(human_readable_bytes(1024 * 1024), "1.0MB");
        assert_eq!(human_readable_bytes(1024 * 1024 * 1024), "1.0GB");
    }

    #[test]
    fn first_n_chars_keeps_prefix_and_escapes_newlines() {
        assert_eq!(first_n_chars(&"abcdef".to_string(), 3), "abc...");
        assert_eq!(first_n_chars(&"ab".to_string(), 3), "ab");
        assert_eq!(first_n_chars(&"a\nb".to_string(), 10), "a\\nb");
    }

    #[test]
    fn last_n_chars_keeps_suffix_and_escapes_newlines() {
        assert_eq!(last_n_chars(&"abcdef".to_string(), 3), "...def");
        assert_eq!(last_n_chars(&"ab".to_string(), 3), "ab");
        assert_eq!(last_n_chars(&"a\nb".to_string(), 10), "a\\nb");
    }
}
