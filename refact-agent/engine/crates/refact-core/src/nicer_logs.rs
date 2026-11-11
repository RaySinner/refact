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
}
