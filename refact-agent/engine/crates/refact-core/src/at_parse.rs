use regex::Regex;

use crate::chat_types::{ContextEnum, ContextFile};

#[derive(Debug, PartialEq)]
pub enum RangeKind {
    GradToCursorTwoSided,
    GradToCursorPrefix,
    GradToCursorSuffix,
    Range,
}

#[derive(Debug, PartialEq)]
pub struct ColonLinesRange {
    pub kind: RangeKind,
    pub line1: usize,
    pub line2: usize,
}

pub fn range_print(range: &ColonLinesRange) -> String {
    match range.kind {
        RangeKind::GradToCursorTwoSided => format!("{}", range.line1),
        RangeKind::GradToCursorPrefix => format!("-{}", range.line2),
        RangeKind::GradToCursorSuffix => format!("{}-", range.line1),
        RangeKind::Range => format!("{}-{}", range.line1, range.line2),
    }
}

pub fn colon_lines_range_from_arg(value: &mut String) -> Option<ColonLinesRange> {
    let re_hyphen = Regex::new(r":(\d+)?-(\d+)?$").unwrap();
    if let Some(captures) = re_hyphen.captures(value.clone().as_str()) {
        *value = re_hyphen.replace(value, "").to_string();
        return match (captures.get(1), captures.get(2)) {
            (Some(line1), Some(line2)) => {
                let line1 = line1.as_str().parse::<usize>().unwrap_or(0);
                let line2 = line2.as_str().parse::<usize>().unwrap_or(0);
                Some(ColonLinesRange {
                    kind: RangeKind::Range,
                    line1,
                    line2,
                })
            }
            (Some(line1), None) => {
                let line1 = line1.as_str().parse::<usize>().unwrap_or(0);
                Some(ColonLinesRange {
                    kind: RangeKind::GradToCursorSuffix,
                    line1,
                    line2: 0,
                })
            }
            (None, Some(line2)) => {
                let line2 = line2.as_str().parse::<usize>().unwrap_or(0);
                Some(ColonLinesRange {
                    kind: RangeKind::GradToCursorPrefix,
                    line1: 0,
                    line2,
                })
            }
            _ => None,
        };
    }
    let re_one_number = Regex::new(r":(\d+)$").unwrap();
    if let Some(captures) = re_one_number.captures(value.clone().as_str()) {
        *value = re_one_number.replace(value, "").to_string();
        if let Some(line1) = captures.get(1) {
            let line = line1.as_str().parse::<usize>().unwrap_or(0);
            return Some(ColonLinesRange {
                kind: RangeKind::GradToCursorTwoSided,
                line1: line,
                line2: 0,
            });
        }
    }
    None
}

pub fn parse_words_from_line(line: &String) -> Vec<(String, usize, usize)> {
    fn trim_punctuation(s: &str) -> &str {
        s.trim_end_matches(&['!', '.', ',', '?'][..])
    }

    let word_regex = Regex::new(r"@?\S+").expect("Invalid regex");

    let mut results = vec![];
    for m in word_regex.find_iter(line) {
        let trimmed = trim_punctuation(m.as_str());
        if !trimmed.is_empty() {
            results.push((trimmed.to_string(), m.start(), m.start() + trimmed.len()));
        }
    }
    results
}

pub fn filter_only_context_file_from_context_tool(tools: &Vec<ContextEnum>) -> Vec<ContextFile> {
    tools
        .iter()
        .filter_map(|x| {
            if let ContextEnum::ContextFile(data) = x {
                Some(data.clone())
            } else {
                None
            }
        })
        .collect::<Vec<ContextFile>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_words_from_line_with_link() {
        let line =
            "Check out this link: https://doc.rust-lang.org/book/ch03-04-comments.html".to_string();
        let parsed_words = parse_words_from_line(&line);

        let link = parsed_words
            .iter()
            .find(|(word, _, _)| word == "https://doc.rust-lang.org/book/ch03-04-comments.html");
        assert!(link.is_some(), "The link should be parsed as a single word");
        if let Some((word, _start, _end)) = link {
            assert_eq!(word, "https://doc.rust-lang.org/book/ch03-04-comments.html");
        }
    }

    #[test]
    fn test_parse_words_from_line_no_empty_tokens() {
        let line = "hello  world    test @file".to_string();
        let parsed_words = parse_words_from_line(&line);

        for (word, _, _) in parsed_words.iter() {
            assert!(!word.is_empty(), "No empty tokens should be produced");
        }
    }

    #[test]
    fn test_parse_words_from_line_long_input() {
        let line = (0..1000).map(|i| format!("word{} ", i)).collect::<String>();
        let parsed_words = parse_words_from_line(&line);

        assert!(
            parsed_words.len() < 2000,
            "Performance regression: too many tokens for long input"
        );
        assert!(
            parsed_words.iter().all(|(w, _, _)| !w.is_empty()),
            "No empty tokens"
        );
    }

    #[test]
    fn test_parse_words_from_line_punctuation_trimming() {
        let line = "@file.txt, src/main.rs! code?".to_string();
        let parsed_words = parse_words_from_line(&line);

        assert_eq!(parsed_words[0].0, "@file.txt");
        assert_eq!(parsed_words[1].0, "src/main.rs");
        assert_eq!(parsed_words[2].0, "code");
    }

    #[test]
    fn test_file_range() {
        {
            let mut value = String::from(":10-20");
            let result = colon_lines_range_from_arg(&mut value);
            assert_eq!(
                result,
                Some(ColonLinesRange {
                    kind: RangeKind::Range,
                    line1: 10,
                    line2: 20
                })
            );
        }
        {
            let mut value = String::from(":5-");
            let result = colon_lines_range_from_arg(&mut value);
            assert_eq!(
                result,
                Some(ColonLinesRange {
                    kind: RangeKind::GradToCursorSuffix,
                    line1: 5,
                    line2: 0
                })
            );
        }
        {
            let mut value = String::from(":-15");
            let result = colon_lines_range_from_arg(&mut value);
            assert_eq!(
                result,
                Some(ColonLinesRange {
                    kind: RangeKind::GradToCursorPrefix,
                    line1: 0,
                    line2: 15
                })
            );
        }
        {
            let mut value = String::from(":25");
            let result = colon_lines_range_from_arg(&mut value);
            assert_eq!(
                result,
                Some(ColonLinesRange {
                    kind: RangeKind::GradToCursorTwoSided,
                    line1: 25,
                    line2: 0
                })
            );
        }
        {
            let mut value = String::from("invalid");
            let result = colon_lines_range_from_arg(&mut value);
            assert_eq!(result, None);
        }
    }
}
