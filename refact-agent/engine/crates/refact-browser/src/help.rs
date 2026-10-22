pub const CHROME_HELP: &str = include_str!("chrome_help.md");

pub const CHROME_HELP_TOPICS: &str = "overview, locators, navigation, input, forms, waits, network, routes, websockets, har, tabs, frames, dialogs, uploads, downloads, emulation, devices, clock, screenshots, artifacts, screencast, assertions, readouts, handlers, cdp, authenticators, reset, troubleshooting";

pub fn chrome_help_text(topic: Option<&str>) -> String {
    let sections = chrome_help_sections();
    let index = format!(
        "chrome help topics: {}\nCall {{\"action\":\"help\",\"topic\":\"<name>\"}} for one section.",
        CHROME_HELP_TOPICS
    );
    let Some(topic) = topic.map(str::trim).filter(|topic| !topic.is_empty()) else {
        let overview = sections
            .iter()
            .find(|(name, _)| name == "overview")
            .map(|(_, body)| body.as_str())
            .unwrap_or_default();
        return format!("{index}\n\n## overview\n{overview}");
    };
    match sections
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(topic))
    {
        Some((name, body)) => format!("## {name}\n{body}"),
        None => format!("Unknown help topic {topic:?}.\n\n{index}"),
    }
}

pub fn chrome_help_sections() -> Vec<(String, String)> {
    let mut sections = Vec::new();
    for chunk in CHROME_HELP.split("\n## ").skip(1) {
        let (name, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
        sections.push((name.trim().to_string(), body.trim().to_string()));
    }
    sections
}
