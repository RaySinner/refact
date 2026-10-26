use serde::{Deserialize, Serialize};

use super::locator::{BrowserLocator, LocatorRegex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TabTarget {
    Active,
    Id { id: String },
}

impl Default for TabTarget {
    fn default() -> Self {
        TabTarget::Active
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilitySnapshotMode {
    #[default]
    Ai,
    Default,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccessibilitySnapshotOptions {
    #[serde(default)]
    pub mode: AccessibilitySnapshotMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<bool>,
    #[serde(default)]
    pub boxes: bool,
    #[serde(default, alias = "root", skip_serializing_if = "Option::is_none")]
    pub locator: Option<BrowserLocator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum BrowserExpectedText {
    Text(String),
    Regex(LocatorRegex),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum BrowserExpectedTextOrList {
    One(BrowserExpectedText),
    Many(Vec<BrowserExpectedText>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPseudoElement {
    Before,
    After,
}

impl BrowserPseudoElement {
    pub fn selector(self) -> &'static str {
        match self {
            Self::Before => "::before",
            Self::After => "::after",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPollMatcher {
    Equals,
    Contains,
    Gt,
    Lt,
    MatchesRegex,
}

impl BrowserPollMatcher {
    pub fn name(self) -> &'static str {
        match self {
            Self::Equals => "equals",
            Self::Contains => "contains",
            Self::Gt => "gt",
            Self::Lt => "lt",
            Self::MatchesRegex => "matches_regex",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserExpectation {
    ToBeAttached,
    ToBeVisible,
    ToBeHidden,
    ToBeEnabled,
    ToBeDisabled,
    ToBeEditable,
    ToBeChecked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indeterminate: Option<bool>,
    },
    ToBeFocused,
    ToBeEmpty,
    ToBeInViewport {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ratio: Option<f64>,
    },
    ToHaveText {
        expected: BrowserExpectedTextOrList,
        #[serde(default)]
        ignore_case: bool,
    },
    ToContainText {
        expected: BrowserExpectedTextOrList,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveValue {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveValues {
        expected: Vec<BrowserExpectedText>,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveAttribute {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected: Option<BrowserExpectedText>,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveClass {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToContainClass {
        expected: String,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveCount {
        expected: usize,
    },
    ToHaveCss {
        name: String,
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pseudo: Option<BrowserPseudoElement>,
    },
    ToHaveId {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveJsProperty {
        name: String,
        expected: serde_json::Value,
    },
    ToHaveRole {
        expected: String,
    },
    ToHaveAccessibleName {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveAccessibleDescription {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveUrl {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToHaveTitle {
        expected: BrowserExpectedText,
        #[serde(default)]
        ignore_case: bool,
    },
    ToMatchAriaSnapshot {
        expected: String,
    },
}

impl BrowserExpectation {
    pub fn name(&self) -> &'static str {
        match self {
            Self::ToBeAttached => "to_be_attached",
            Self::ToBeVisible => "to_be_visible",
            Self::ToBeHidden => "to_be_hidden",
            Self::ToBeEnabled => "to_be_enabled",
            Self::ToBeDisabled => "to_be_disabled",
            Self::ToBeEditable => "to_be_editable",
            Self::ToBeChecked { .. } => "to_be_checked",
            Self::ToBeFocused => "to_be_focused",
            Self::ToBeEmpty => "to_be_empty",
            Self::ToBeInViewport { .. } => "to_be_in_viewport",
            Self::ToHaveText { .. } => "to_have_text",
            Self::ToContainText { .. } => "to_contain_text",
            Self::ToHaveValue { .. } => "to_have_value",
            Self::ToHaveValues { .. } => "to_have_values",
            Self::ToHaveAttribute { .. } => "to_have_attribute",
            Self::ToHaveClass { .. } => "to_have_class",
            Self::ToContainClass { .. } => "to_contain_class",
            Self::ToHaveCount { .. } => "to_have_count",
            Self::ToHaveCss { .. } => "to_have_css",
            Self::ToHaveId { .. } => "to_have_id",
            Self::ToHaveJsProperty { .. } => "to_have_js_property",
            Self::ToHaveRole { .. } => "to_have_role",
            Self::ToHaveAccessibleName { .. } => "to_have_accessible_name",
            Self::ToHaveAccessibleDescription { .. } => "to_have_accessible_description",
            Self::ToHaveUrl { .. } => "to_have_url",
            Self::ToHaveTitle { .. } => "to_have_title",
            Self::ToMatchAriaSnapshot { .. } => "to_match_aria_snapshot",
        }
    }

    pub fn requires_locator(&self) -> bool {
        !matches!(self, Self::ToHaveUrl { .. } | Self::ToHaveTitle { .. })
    }

    pub fn is_multi_element(&self) -> bool {
        match self {
            Self::ToHaveCount { .. } | Self::ToHaveValues { .. } => true,
            Self::ToHaveText { expected, .. } | Self::ToContainText { expected, .. } => {
                matches!(expected, BrowserExpectedTextOrList::Many(_))
            }
            _ => false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::ToBeChecked {
                checked: Some(_),
                indeterminate: Some(true),
            } => Err(
                "to_be_checked cannot combine indeterminate: true with a checked expectation"
                    .to_string(),
            ),
            Self::ToBeInViewport { ratio: Some(ratio) } if !(0.0..=1.0).contains(ratio) => Err(
                format!("to_be_in_viewport ratio must be between 0 and 1, got {ratio}"),
            ),
            _ => Ok(()),
        }
    }
}

impl AccessibilitySnapshotOptions {
    pub fn refs_enabled(&self) -> bool {
        self.refs
            .unwrap_or(matches!(self.mode, AccessibilitySnapshotMode::Ai))
    }
}
