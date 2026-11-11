pub const COMMENT_CAP: usize = 500;
pub const COMMENT_BODY_MAX_CHARS: usize = 4000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_cap_is_500() {
        assert_eq!(COMMENT_CAP, 500);
    }

    #[test]
    fn comment_body_max_chars_is_4000() {
        assert_eq!(COMMENT_BODY_MAX_CHARS, 4000);
    }
}
