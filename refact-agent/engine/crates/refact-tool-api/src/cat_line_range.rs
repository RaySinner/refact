pub type CatLineRange = (usize, usize);

/// Parse the `:START` / `:START-END` suffix a `cat` caller may append to a path.
///
/// Lives at module scope (not nested inside `parse_cat_args`) so the loop guard's
/// `cat` identity can strip line ranges with the exact same parser this tool uses,
/// and the two can never drift apart. Behaviour is unchanged.
pub fn try_parse_line_range(s: &str) -> Result<Option<CatLineRange>, String> {
    let s = s.trim();

    // Try parsing as a single number (like "10")
    if let Ok(n) = s.parse::<usize>() {
        return Ok(Some((n, n)));
    }

    // Try parsing as a range (like "10-20")
    if s.contains('-') {
        let parts = s.split('-').collect::<Vec<_>>();
        if parts.len() == 2 {
            if let Ok(start) = parts[0].trim().parse::<usize>() {
                if let Ok(end) = parts[1].trim().parse::<usize>() {
                    if start > end {
                        return Err(format!(
                            "Start line ({}) cannot be greater than end line ({})",
                            start, end
                        ));
                    }
                    return Ok(Some((start, end)));
                }
            }
        }
    }

    Ok(None) // Not a line range - likely a Windows path
}
