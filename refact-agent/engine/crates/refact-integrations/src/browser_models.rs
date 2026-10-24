use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::browser_types::{ConsoleEntry, NetworkEntry};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketRouteMode {
    #[default]
    Mock,
    Intercept,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketMessageAction {
    #[default]
    Forward,
    Drop,
    Capture,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketFrameDisposition {
    Forwarded,
    Captured,
    Dropped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketEventKind {
    Created,
    HandshakeResponse,
    FrameSent,
    FrameReceived,
    Closed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSocketEvent {
    pub sequence: u64,
    pub socket_id: String,
    pub url: String,
    pub kind: WebSocketEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opcode: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub routed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<WebSocketFrameDisposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarMode {
    #[default]
    Full,
    Minimal,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarContentPolicy {
    #[default]
    Omit,
    Embed,
    Attach,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarNotFound {
    Abort,
    Fallback,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAuthenticatorProtocol {
    U2f,
    #[default]
    Ctap2,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAuthenticatorTransport {
    #[default]
    Usb,
    Nfc,
    Ble,
    Cable,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserWebAuthnCredential {
    pub credential_id: String,
    #[serde(default)]
    pub is_resident_credential: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rp_id: Option<String>,
    pub private_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_handle: Option<String>,
    #[serde(default)]
    pub sign_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub large_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_eligibility: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_state: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocatorRegex {
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub flags: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum AriaCheckedState {
    Bool(bool),
    Mixed(AriaMixedState),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AriaMixedState {
    #[serde(rename = "mixed")]
    Mixed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocatorStrategy {
    Ref {
        value: String,
    },
    Css {
        value: String,
    },
    Id {
        value: String,
    },
    Name {
        value: String,
    },
    TestId {
        value: String,
        exact: Option<bool>,
        regex: Option<LocatorRegex>,
        attribute: Option<String>,
    },
    Placeholder {
        value: String,
        exact: Option<bool>,
        regex: Option<LocatorRegex>,
    },
    AltText {
        value: String,
        exact: Option<bool>,
        regex: Option<LocatorRegex>,
    },
    Title {
        value: String,
        exact: Option<bool>,
        regex: Option<LocatorRegex>,
    },
    Autocomplete {
        value: String,
    },
    Text {
        value: String,
        exact: bool,
        regex: Option<LocatorRegex>,
    },
    Label {
        value: String,
        exact: Option<bool>,
        regex: Option<LocatorRegex>,
    },
    Role {
        role: String,
        name: Option<String>,
        description: Option<String>,
        exact: Option<bool>,
        name_regex: Option<LocatorRegex>,
        description_regex: Option<LocatorRegex>,
        checked: Option<AriaCheckedState>,
        pressed: Option<AriaCheckedState>,
        selected: Option<bool>,
        expanded: Option<bool>,
        disabled: Option<bool>,
        level: Option<u32>,
        include_hidden: Option<bool>,
    },
    Xpath {
        value: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
enum LocatorWire {
    Ref {
        value: String,
    },
    Css {
        value: String,
    },
    Id {
        value: String,
    },
    Name {
        value: String,
    },
    TestId {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<LocatorRegex>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribute: Option<String>,
    },
    Placeholder {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<LocatorRegex>,
    },
    AltText {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<LocatorRegex>,
    },
    Title {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<LocatorRegex>,
    },
    Autocomplete {
        value: String,
    },
    Text {
        value: String,
        #[serde(default, skip_serializing_if = "is_false")]
        exact: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<LocatorRegex>,
    },
    Label {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<LocatorRegex>,
    },
    Role {
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name_regex: Option<LocatorRegex>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_regex: Option<LocatorRegex>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked: Option<AriaCheckedState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressed: Option<AriaCheckedState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expanded: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_hidden: Option<bool>,
    },
    Xpath {
        value: String,
    },
}

pub fn locator_strategy_from_wire(
    value: serde_json::Value,
) -> Result<LocatorStrategy, serde_json::Error> {
    serde_json::from_value(value)
}

impl Serialize for LocatorStrategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let wire = match self {
            Self::Ref { value } => LocatorWire::Ref {
                value: value.clone(),
            },
            Self::Css { value } => LocatorWire::Css {
                value: value.clone(),
            },
            Self::Id { value } => LocatorWire::Id {
                value: value.clone(),
            },
            Self::Name { value } => LocatorWire::Name {
                value: value.clone(),
            },
            Self::TestId {
                value,
                exact,
                regex,
                attribute,
            } => LocatorWire::TestId {
                value: value.clone(),
                exact: *exact,
                regex: regex.clone(),
                attribute: attribute.clone(),
            },
            Self::Placeholder {
                value,
                exact,
                regex,
            } => LocatorWire::Placeholder {
                value: value.clone(),
                exact: *exact,
                regex: regex.clone(),
            },
            Self::AltText {
                value,
                exact,
                regex,
            } => LocatorWire::AltText {
                value: value.clone(),
                exact: *exact,
                regex: regex.clone(),
            },
            Self::Title {
                value,
                exact,
                regex,
            } => LocatorWire::Title {
                value: value.clone(),
                exact: *exact,
                regex: regex.clone(),
            },
            Self::Autocomplete { value } => LocatorWire::Autocomplete {
                value: value.clone(),
            },
            Self::Text {
                value,
                exact,
                regex,
            } => LocatorWire::Text {
                value: value.clone(),
                exact: *exact,
                regex: regex.clone(),
            },
            Self::Label {
                value,
                exact,
                regex,
            } => LocatorWire::Label {
                value: value.clone(),
                exact: *exact,
                regex: regex.clone(),
            },
            Self::Role {
                role,
                name,
                description,
                exact,
                name_regex,
                description_regex,
                checked,
                pressed,
                selected,
                expanded,
                disabled,
                level,
                include_hidden,
            } => LocatorWire::Role {
                role: role.clone(),
                name: name.clone(),
                description: description.clone(),
                exact: *exact,
                name_regex: name_regex.clone(),
                description_regex: description_regex.clone(),
                checked: *checked,
                pressed: *pressed,
                selected: *selected,
                expanded: *expanded,
                disabled: *disabled,
                level: *level,
                include_hidden: *include_hidden,
            },
            Self::Xpath { value } => LocatorWire::Xpath {
                value: value.clone(),
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LocatorStrategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = LocatorWire::deserialize(deserializer)?;
        Ok(match wire {
            LocatorWire::Ref { value } => Self::Ref { value },
            LocatorWire::Css { value } => Self::Css { value },
            LocatorWire::Id { value } => Self::Id { value },
            LocatorWire::Name { value } => Self::Name { value },
            LocatorWire::TestId {
                value,
                exact,
                regex,
                attribute,
            } => Self::TestId {
                value,
                exact,
                regex,
                attribute,
            },
            LocatorWire::Placeholder {
                value,
                exact,
                regex,
            } => Self::Placeholder {
                value,
                exact,
                regex,
            },
            LocatorWire::AltText {
                value,
                exact,
                regex,
            } => Self::AltText {
                value,
                exact,
                regex,
            },
            LocatorWire::Title {
                value,
                exact,
                regex,
            } => Self::Title {
                value,
                exact,
                regex,
            },
            LocatorWire::Autocomplete { value } => Self::Autocomplete { value },
            LocatorWire::Text {
                value,
                exact,
                regex,
            } => Self::Text {
                value,
                exact,
                regex,
            },
            LocatorWire::Label {
                value,
                exact,
                regex,
            } => Self::Label {
                value,
                exact,
                regex,
            },
            LocatorWire::Role {
                role,
                name,
                description,
                exact,
                name_regex,
                description_regex,
                checked,
                pressed,
                selected,
                expanded,
                disabled,
                level,
                include_hidden,
            } => Self::Role {
                role,
                name,
                description,
                exact,
                name_regex,
                description_regex,
                checked,
                pressed,
                selected,
                expanded,
                disabled,
                level,
                include_hidden,
            },
            LocatorWire::Xpath { value } => Self::Xpath { value },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LocatorTextMatcher {
    Text(String),
    Regex(LocatorRegex),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LocatorFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has: Option<Box<BrowserLocator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_not: Option<Box<BrowserLocator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_text: Option<LocatorTextMatcher>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_not_text: Option<LocatorTextMatcher>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserLocator {
    #[serde(flatten)]
    pub strategy: LocatorStrategy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<BrowserLocator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<isize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<Box<BrowserLocator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<LocatorFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub and: Option<Box<BrowserLocator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub or: Option<Box<BrowserLocator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<bool>,
}

impl BrowserLocator {
    pub fn reference(reference: &str) -> Self {
        Self {
            strategy: LocatorStrategy::Ref {
                value: reference.to_string(),
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    pub fn css(selector: &str) -> Self {
        Self {
            strategy: LocatorStrategy::Css {
                value: selector.to_string(),
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    #[allow(dead_code)]
    pub fn id(id: &str) -> Self {
        Self {
            strategy: LocatorStrategy::Id {
                value: id.to_string(),
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    #[allow(dead_code)]
    pub fn name(name: &str) -> Self {
        Self {
            strategy: LocatorStrategy::Name {
                value: name.to_string(),
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    #[allow(dead_code)]
    pub fn label(label: &str) -> Self {
        Self {
            strategy: LocatorStrategy::Label {
                value: label.to_string(),
                exact: None,
                regex: None,
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    #[allow(dead_code)]
    pub fn placeholder(ph: &str) -> Self {
        Self {
            strategy: LocatorStrategy::Placeholder {
                value: ph.to_string(),
                exact: None,
                regex: None,
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    #[allow(dead_code)]
    pub fn role(role: &str, name: Option<&str>) -> Self {
        Self {
            strategy: LocatorStrategy::Role {
                role: role.to_string(),
                name: name.map(|s| s.to_string()),
                description: None,
                exact: None,
                name_regex: None,
                description_regex: None,
                checked: None,
                pressed: None,
                selected: None,
                expanded: None,
                disabled: None,
                level: None,
                include_hidden: None,
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    #[allow(dead_code)]
    pub fn test_id(tid: &str) -> Self {
        Self {
            strategy: LocatorStrategy::TestId {
                value: tid.to_string(),
                exact: None,
                regex: None,
                attribute: None,
            },
            frames: Vec::new(),
            nth: None,
            within: None,
            locator: None,
            filter: None,
            and: None,
            or: None,
            first: None,
            last: None,
        }
    }

    pub fn in_frames(mut self, frames: Vec<BrowserLocator>) -> Self {
        self.frames = frames;
        self
    }
}

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotType {
    #[default]
    Png,
    Jpeg,
    Webp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotScale {
    Css,
    #[default]
    Device,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotAnimations {
    #[default]
    Allow,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserScreenshotCaret {
    #[default]
    Hide,
    Initial,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BrowserScreenshotClip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserHttpRequest {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_json: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_on_status: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_headers: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserScreenshotOptions {
    #[serde(default, skip_serializing_if = "is_false")]
    pub full_page: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<BrowserScreenshotClip>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub image_type: Option<BrowserScreenshotType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<BrowserScreenshotScale>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub omit_background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animations: Option<BrowserScreenshotAnimations>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caret: Option<BrowserScreenshotCaret>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask: Vec<BrowserLocator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserComposeMode {
    #[default]
    Grid,
    Separate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrowserElementState {
    #[default]
    Default,
    Hover,
    Focus,
    Active,
}

impl BrowserElementState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Hover => "hover",
            Self::Focus => "focus",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserPdfMargin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BrowserPdfOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landscape: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub print_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margins: Option<BrowserPdfMargin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_ranges: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_css_page_size: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserPosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMouseButton {
    #[default]
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserStep {
    Navigate {
        url: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Reload,
    GoBack,
    GoForward,

    OpenTab {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
    CloseTab {
        #[serde(default)]
        tab: Option<TabTarget>,
    },
    SwitchTab {
        tab: TabTarget,
    },
    ListTabs,
    WaitForPopup {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },

    Route {
        pattern: UrlPattern,
        handler: RouteHandler,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        times: Option<u32>,
    },
    Unroute {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<UrlPattern>,
    },
    ListRoutes,

    RouteWebSocket {
        pattern: UrlPattern,
        #[serde(default)]
        mode: WebSocketRouteMode,
        #[serde(default)]
        on_page_message: WebSocketMessageAction,
        #[serde(default)]
        on_server_message: WebSocketMessageAction,
    },
    UnrouteWebSocket {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<UrlPattern>,
    },
    SendWebSocketMessage {
        #[serde(alias = "url_pattern")]
        pattern: UrlPattern,
        #[serde(alias = "data")]
        text: String,
    },
    CloseWebSocket {
        #[serde(alias = "url_pattern")]
        pattern: UrlPattern,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    WaitForWebSocketFrame {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<UrlPattern>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },

    StartHarRecording {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default)]
        mode: HarMode,
        #[serde(default)]
        content: HarContentPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_filter: Option<UrlPattern>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update: Option<String>,
    },
    StopHarRecording,
    RouteFromHar {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_filter: Option<UrlPattern>,
        not_found: HarNotFound,
    },
    Reset,
    CdpSend {
        method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
        #[serde(default)]
        target: CdpTarget,
    },
    HttpRequest {
        #[serde(flatten)]
        options: BrowserHttpRequest,
    },

    ClockInstall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<ClockTime>,
    },
    ClockFastForward {
        ticks: ClockTicks,
    },
    ClockPauseAt {
        time: ClockTime,
    },
    ClockResume,
    ClockRunFor {
        ticks: ClockTicks,
    },
    ClockSetFixedTime {
        time: ClockTime,
    },
    ClockSetSystemTime {
        time: ClockTime,
    },

    StartCoverage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        js: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        css: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_on_navigation: Option<bool>,
    },
    StopCoverage,

    CaptureFrames {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_count: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_page: Option<bool>,
    },
    ScreencastStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_height: Option<u32>,
    },
    ScreencastStop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compose: Option<bool>,
    },

    AddVirtualAuthenticator {
        #[serde(
            default,
            skip_serializing,
            deserialize_with = "reject_server_minted_authenticator_id"
        )]
        id: (),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<BrowserAuthenticatorProtocol>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transport: Option<BrowserAuthenticatorTransport>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_resident_key: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_user_verification: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_user_verified: Option<bool>,
    },
    RemoveVirtualAuthenticator {
        id: String,
    },
    ListCredentials {
        id: String,
    },
    AddCredential {
        id: String,
        credential: BrowserWebAuthnCredential,
    },
    ClearCredentials {
        id: String,
    },
    SetUserVerified {
        id: String,
        verified: bool,
    },

    SetViewport {
        width: u32,
        height: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_scale_factor: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_mobile: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        has_touch: Option<bool>,
    },
    SetWindowBounds {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    EmulateMedia {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color_scheme: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reduced_motion: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forced_colors: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contrast: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media: Option<String>,
    },
    SetLocale {
        locale: String,
    },
    SetTimezone {
        timezone: String,
    },
    SetUserAgent {
        user_agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accept_language: Option<String>,
    },
    SetGeolocation {
        latitude: f64,
        longitude: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accuracy: Option<f64>,
    },
    SetOffline {
        offline: bool,
    },
    SetNetworkConditions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latency_ms: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        download_kbps: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upload_kbps: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
    SetCpuThrottling {
        rate: f64,
    },
    EmulateDevice {
        name: String,
    },
    ListDevices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    SetExtraHttpHeaders {
        headers: BTreeMap<String, String>,
    },
    GetCookies {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        urls: Option<Vec<String>>,
    },
    SetCookies {
        cookies: Vec<BrowserCookie>,
    },
    ClearCookies {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        domain: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    GetStorage {
        kind: BrowserStorageKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
    SetStorage {
        kind: BrowserStorageKind,
        items: Vec<BrowserStorageItem>,
    },
    ClearStorage {
        kind: BrowserStorageKind,
    },
    StorageState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        save_as: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indexed_db: Option<bool>,
    },
    SetStorageState {
        state: BrowserStorageState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        indexed_db: Option<bool>,
    },
    GrantPermissions {
        permissions: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        #[serde(default, skip_serializing_if = "BrowserPermissionState::is_granted")]
        state: BrowserPermissionState,
    },
    ClearPermissions,
    SetHttpCredentials {
        username: String,
        password: String,
    },

    Expect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        matcher: BrowserExpectation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default)]
        soft: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not: Option<bool>,
    },
    ExpectPoll {
        expression: String,
        expected: serde_json::Value,
        matcher: BrowserPollMatcher,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        soft: Option<bool>,
    },

    Click {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    ClickIfExists {
        locator: BrowserLocator,
    },
    Tap {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
    },
    Hover {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    Focus {
        locator: BrowserLocator,
    },
    Blur {
        locator: BrowserLocator,
    },
    ScrollTo {
        locator: BrowserLocator,
    },
    PressKey {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    InsertText {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        text: String,
    },
    PressSequentially {
        locator: BrowserLocator,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u64>,
    },
    DragAndDrop {
        source: BrowserLocator,
        target: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_position: Option<BrowserPosition>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_position: Option<BrowserPosition>,
    },
    DropFiles {
        target: BrowserLocator,
        paths: Vec<String>,
    },
    MouseMove {
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steps: Option<usize>,
    },
    MouseDown {
        #[serde(default)]
        button: BrowserMouseButton,
    },
    MouseUp {
        #[serde(default)]
        button: BrowserMouseButton,
    },
    MouseClickXy {
        x: f64,
        y: f64,
        #[serde(default)]
        button: BrowserMouseButton,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        click_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay: Option<u64>,
    },
    MouseDragXy {
        start_x: f64,
        start_y: f64,
        end_x: f64,
        end_y: f64,
    },
    MouseWheel {
        delta_x: f64,
        delta_y: f64,
    },

    Fill {
        locator: BrowserLocator,
        text: String,
        #[serde(default = "default_true")]
        clear_first: bool,
        #[serde(default = "default_true")]
        verify: bool,
    },
    Clear {
        locator: BrowserLocator,
        #[serde(default = "default_true")]
        verify: bool,
    },
    SelectOption {
        locator: BrowserLocator,
        value: String,
    },
    Check {
        locator: BrowserLocator,
    },
    Uncheck {
        locator: BrowserLocator,
    },
    SetInputFiles {
        locator: BrowserLocator,
        paths: Vec<String>,
    },
    ExpectFileChooser {
        paths: Vec<String>,
    },

    WaitForFunction {
        expression: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locator: Option<BrowserLocator>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        polling_ms: Option<u64>,
    },
    WaitForSelector {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<BrowserWaitState>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForNavigation {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForUrl {
        #[serde(alias = "contains")]
        pattern: UrlPattern,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForText {
        text: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForNetworkIdle {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForLoadState {
        state: BrowserLoadState,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForRequest {
        #[serde(alias = "url_or_pattern")]
        pattern: UrlPattern,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForResponse {
        #[serde(alias = "url_or_pattern")]
        pattern: UrlPattern,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForConsoleMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<BrowserConsoleLevel>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForDownload {
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        save_as: Option<String>,
    },
    CancelDownload {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    WaitForElementHidden {
        locator: BrowserLocator,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitForElementStable {
        locator: BrowserLocator,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    WaitSeconds {
        seconds: f64,
    },

    GetText {
        locator: BrowserLocator,
    },
    GetHtml {
        locator: BrowserLocator,
    },
    GetAttribute {
        locator: BrowserLocator,
        attribute: String,
    },
    BoundingBox {
        locator: BrowserLocator,
    },
    Count {
        locator: BrowserLocator,
    },
    InputValue {
        locator: BrowserLocator,
    },
    AllTexts {
        locator: BrowserLocator,
        #[serde(default)]
        mode: BrowserTextMode,
        #[serde(default)]
        limit: Option<usize>,
    },
    ElementState {
        locator: BrowserLocator,
    },
    ExtractLinks {
        #[serde(default)]
        locator: Option<BrowserLocator>,
        #[serde(default)]
        limit: Option<usize>,
    },
    ExtractTable {
        locator: BrowserLocator,
        #[serde(default)]
        limit: Option<usize>,
    },
    DomSnapshot {
        selector: String,
        #[serde(default)]
        max_chars: Option<usize>,
    },
    AccessibilitySnapshot {
        #[serde(flatten)]
        options: AccessibilitySnapshotOptions,
    },
    Screenshot {
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    ScreenshotElement {
        locator: BrowserLocator,
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    ScreenshotElements {
        locators: Vec<BrowserLocator>,
        #[serde(default)]
        compose: BrowserComposeMode,
        #[serde(default)]
        labels: Option<bool>,
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    CaptureElementStates {
        locator: BrowserLocator,
        #[serde(default)]
        states: Vec<BrowserElementState>,
        #[serde(default)]
        labels: Option<bool>,
        #[serde(flatten)]
        options: BrowserScreenshotOptions,
    },
    Pdf {
        #[serde(flatten)]
        options: BrowserPdfOptions,
    },

    Eval {
        expression: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    Styles {
        locator: BrowserLocator,
        #[serde(default)]
        property_filter: Option<String>,
    },

    SetContent {
        html: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_until: Option<BrowserLoadState>,
    },
    PageContent,
    AddScriptTag {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script_type: Option<String>,
    },
    AddStyleTag {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    AddInitScript {
        content: String,
    },
    RemoveInitScript {
        id: String,
    },
    DispatchEvent {
        locator: BrowserLocator,
        event_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_init: Option<serde_json::Value>,
    },

    TabLog,

    AddLocatorHandler {
        name: String,
        locator: BrowserLocator,
        handler: LocatorHandlerAction,
        #[serde(default)]
        times: Option<u32>,
        #[serde(default)]
        no_wait_after: bool,
    },
    RemoveLocatorHandler {
        name: String,
    },
    HandleDialog {
        accept: bool,
        #[serde(default)]
        prompt_text: Option<String>,
    },

    DismissOverlays {
        #[serde(default)]
        aggressive: bool,
    },
    HighlightElement {
        locator: BrowserLocator,
    },
    Highlight {
        locator: BrowserLocator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    HideHighlight,
    Annotate {
        locator: BrowserLocator,
        text: String,
    },

    Help {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocatorHandlerAction {
    Click,
    Steps { steps: Vec<BrowserStep> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteHandler {
    Fulfill {
        #[serde(default = "default_fulfill_status")]
        status: u16,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        json: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        body_base64: bool,
    },
    Abort {
        reason: String,
    },
    Continue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<BTreeMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_data: Option<String>,
    },
    Fallback,
    FetchAndFulfill {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<BTreeMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        response_headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        body_base64: bool,
    },
}

fn default_fulfill_status() -> u16 {
    200
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLoadState {
    Domcontentloaded,
    Load,
    Networkidle,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserWaitState {
    #[default]
    Attached,
    Detached,
    Visible,
    Hidden,
}

impl BrowserWaitState {
    pub fn needs_visibility(self) -> bool {
        matches!(self, Self::Visible | Self::Hidden)
    }

    pub fn reached_label(self) -> &'static str {
        match self {
            Self::Attached => "Element found",
            Self::Detached => "Element detached",
            Self::Visible => "Element visible",
            Self::Hidden => "Element hidden",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserConsoleLevel {
    Log,
    Warning,
    Error,
}

impl BrowserConsoleLevel {
    pub fn matches_level(self, level: &str) -> bool {
        match self {
            Self::Log => level.eq_ignore_ascii_case("log"),
            Self::Warning => level.eq_ignore_ascii_case("warning"),
            Self::Error => {
                level.eq_ignore_ascii_case("error") || level.eq_ignore_ascii_case("page_error")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTextMode {
    #[default]
    InnerText,
    TextContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum UrlPattern {
    Text(String),
    Regex {
        source: String,
        #[serde(default)]
        flags: String,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CdpTarget {
    #[default]
    Page,
    Browser,
}

impl CdpTarget {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ClockTicks {
    Millis(i64),
    Human(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ClockTime {
    UnixMillis(i64),
    Text(String),
}

fn default_true() -> bool {
    true
}

fn reject_server_minted_authenticator_id<'de, D>(_deserializer: D) -> Result<(), D::Error>
where
    D: Deserializer<'de>,
{
    Err(serde::de::Error::custom(
        "id is server-minted; omit it and use the returned id",
    ))
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl BrowserStep {
    pub const ACTION_NAMES: &'static [&'static str] = &[
        "navigate",
        "reload",
        "go_back",
        "go_forward",
        "open_tab",
        "close_tab",
        "switch_tab",
        "list_tabs",
        "wait_for_popup",
        "route",
        "unroute",
        "list_routes",
        "route_web_socket",
        "unroute_web_socket",
        "send_web_socket_message",
        "close_web_socket",
        "wait_for_web_socket_frame",
        "start_har_recording",
        "stop_har_recording",
        "route_from_har",
        "reset",
        "cdp_send",
        "clock_install",
        "clock_fast_forward",
        "clock_pause_at",
        "clock_resume",
        "clock_run_for",
        "clock_set_fixed_time",
        "clock_set_system_time",
        "http_request",
        "start_coverage",
        "stop_coverage",
        "capture_frames",
        "screencast_start",
        "screencast_stop",
        "add_virtual_authenticator",
        "remove_virtual_authenticator",
        "list_credentials",
        "add_credential",
        "clear_credentials",
        "set_user_verified",
        "set_viewport",
        "set_window_bounds",
        "emulate_media",
        "set_locale",
        "set_timezone",
        "set_user_agent",
        "set_geolocation",
        "set_offline",
        "set_network_conditions",
        "set_cpu_throttling",
        "emulate_device",
        "list_devices",
        "set_extra_http_headers",
        "get_cookies",
        "set_cookies",
        "clear_cookies",
        "get_storage",
        "set_storage",
        "clear_storage",
        "storage_state",
        "set_storage_state",
        "grant_permissions",
        "clear_permissions",
        "set_http_credentials",
        "expect",
        "expect_poll",
        "click",
        "click_if_exists",
        "tap",
        "hover",
        "focus",
        "blur",
        "scroll_to",
        "press_key",
        "insert_text",
        "press_sequentially",
        "drag_and_drop",
        "drop_files",
        "mouse_move",
        "mouse_down",
        "mouse_up",
        "mouse_click_xy",
        "mouse_drag_xy",
        "mouse_wheel",
        "fill",
        "clear",
        "select_option",
        "check",
        "uncheck",
        "set_input_files",
        "expect_file_chooser",
        "wait_for_function",
        "wait_for_selector",
        "wait_for_navigation",
        "wait_for_url",
        "wait_for_text",
        "wait_for_network_idle",
        "wait_for_load_state",
        "wait_for_request",
        "wait_for_response",
        "wait_for_console_message",
        "wait_for_download",
        "cancel_download",
        "wait_for_element_hidden",
        "wait_for_element_stable",
        "wait_seconds",
        "get_text",
        "get_html",
        "get_attribute",
        "bounding_box",
        "count",
        "input_value",
        "all_texts",
        "element_state",
        "extract_links",
        "extract_table",
        "dom_snapshot",
        "accessibility_snapshot",
        "screenshot",
        "screenshot_element",
        "screenshot_elements",
        "capture_element_states",
        "pdf",
        "eval",
        "styles",
        "set_content",
        "page_content",
        "add_script_tag",
        "add_style_tag",
        "add_init_script",
        "remove_init_script",
        "dispatch_event",
        "tab_log",
        "add_locator_handler",
        "remove_locator_handler",
        "handle_dialog",
        "dismiss_overlays",
        "highlight_element",
        "highlight",
        "hide_highlight",
        "annotate",
        "help",
    ];
}

/// Per-action serde contract for [`BrowserStep`]: `(action, required, optional, aliases)`.
/// The published JSON schema is generated from this table, so an advertised field is a
/// field the deserializer actually accepts. `browser_step_fields_match_the_serde_contract`
/// pins it against real round-trips.
pub type BrowserActionFields = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
);

impl BrowserStep {
    pub const ACTION_FIELDS: &'static [BrowserActionFields] = &[
        ("navigate", &["url"], &["timeout_ms"], &[]),
        ("reload", &[], &[], &[]),
        ("go_back", &[], &[], &[]),
        ("go_forward", &[], &[], &[]),
        ("open_tab", &[], &["device", "url"], &[]),
        ("close_tab", &[], &["tab"], &[]),
        ("switch_tab", &["tab"], &[], &[]),
        ("list_tabs", &[], &[], &[]),
        ("wait_for_popup", &[], &["timeout_ms"], &[]),
        ("route", &["handler", "pattern"], &["times"], &[]),
        ("unroute", &[], &["pattern"], &[]),
        ("list_routes", &[], &[], &[]),
        (
            "route_web_socket",
            &["pattern"],
            &["mode", "on_page_message", "on_server_message"],
            &[],
        ),
        ("unroute_web_socket", &[], &["pattern"], &[]),
        (
            "send_web_socket_message",
            &["pattern", "text"],
            &[],
            &["data", "url_pattern"],
        ),
        (
            "close_web_socket",
            &["pattern"],
            &["code", "reason"],
            &["url_pattern"],
        ),
        (
            "wait_for_web_socket_frame",
            &[],
            &["pattern", "timeout_ms"],
            &[],
        ),
        (
            "start_har_recording",
            &["content", "mode"],
            &["path", "update", "url_filter"],
            &[],
        ),
        ("stop_har_recording", &[], &[], &[]),
        (
            "route_from_har",
            &["not_found", "path"],
            &["url_filter"],
            &[],
        ),
        ("reset", &[], &[], &[]),
        ("cdp_send", &["method"], &["params", "target"], &[]),
        (
            "http_request",
            &["url"],
            &[
                "body",
                "body_json",
                "fail_on_status",
                "form",
                "full_headers",
                "headers",
                "max_redirects",
                "method",
                "timeout_ms",
            ],
            &[],
        ),
        ("clock_install", &[], &["time"], &[]),
        ("clock_fast_forward", &["ticks"], &[], &[]),
        ("clock_pause_at", &["time"], &[], &[]),
        ("clock_resume", &[], &[], &[]),
        ("clock_run_for", &["ticks"], &[], &[]),
        ("clock_set_fixed_time", &["time"], &[], &[]),
        ("clock_set_system_time", &["time"], &[], &[]),
        (
            "start_coverage",
            &[],
            &["css", "js", "reset_on_navigation"],
            &[],
        ),
        ("stop_coverage", &[], &[], &[]),
        (
            "capture_frames",
            &[],
            &[
                "duration_ms",
                "frame_count",
                "full_page",
                "interval_ms",
                "locator",
            ],
            &[],
        ),
        (
            "screencast_start",
            &[],
            &["max_height", "max_width", "quality"],
            &[],
        ),
        ("screencast_stop", &[], &["compose"], &[]),
        (
            "add_virtual_authenticator",
            &[],
            &[
                "has_resident_key",
                "has_user_verification",
                "is_user_verified",
                "protocol",
                "transport",
            ],
            &[],
        ),
        ("remove_virtual_authenticator", &["id"], &[], &[]),
        ("list_credentials", &["id"], &[], &[]),
        ("add_credential", &["credential", "id"], &[], &[]),
        ("clear_credentials", &["id"], &[], &[]),
        ("set_user_verified", &["id", "verified"], &[], &[]),
        (
            "set_viewport",
            &["height", "width"],
            &["device_scale_factor", "has_touch", "is_mobile"],
            &[],
        ),
        (
            "set_window_bounds",
            &[],
            &["height", "width", "x", "y"],
            &[],
        ),
        (
            "emulate_media",
            &[],
            &[
                "color_scheme",
                "contrast",
                "forced_colors",
                "media",
                "reduced_motion",
            ],
            &[],
        ),
        ("set_locale", &["locale"], &[], &[]),
        ("set_timezone", &["timezone"], &[], &[]),
        ("set_user_agent", &["user_agent"], &["accept_language"], &[]),
        (
            "set_geolocation",
            &["latitude", "longitude"],
            &["accuracy"],
            &[],
        ),
        ("set_offline", &["offline"], &[], &[]),
        (
            "set_network_conditions",
            &[],
            &[
                "download_kbps",
                "latency_ms",
                "offline",
                "preset",
                "upload_kbps",
            ],
            &[],
        ),
        ("set_cpu_throttling", &["rate"], &[], &[]),
        ("emulate_device", &["name"], &[], &[]),
        ("list_devices", &[], &["filter"], &[]),
        ("set_extra_http_headers", &["headers"], &[], &[]),
        ("get_cookies", &[], &["urls"], &[]),
        ("set_cookies", &["cookies"], &[], &[]),
        ("clear_cookies", &[], &["domain", "name", "path"], &[]),
        ("get_storage", &["kind"], &["origin"], &[]),
        ("set_storage", &["items", "kind"], &[], &[]),
        ("clear_storage", &["kind"], &[], &[]),
        ("storage_state", &[], &["indexed_db", "save_as"], &[]),
        ("set_storage_state", &["state"], &["indexed_db"], &[]),
        (
            "grant_permissions",
            &["permissions"],
            &["origin", "state"],
            &[],
        ),
        ("clear_permissions", &[], &[], &[]),
        ("set_http_credentials", &["password", "username"], &[], &[]),
        (
            "expect",
            &["matcher"],
            &["locator", "not", "soft", "timeout_ms"],
            &[],
        ),
        (
            "expect_poll",
            &["expected", "expression", "matcher"],
            &["soft", "timeout_ms"],
            &[],
        ),
        ("click", &["locator"], &["timeout_ms"], &[]),
        ("click_if_exists", &["locator"], &[], &[]),
        ("tap", &[], &["locator", "x", "y"], &[]),
        ("hover", &["locator"], &["timeout_ms"], &[]),
        ("focus", &["locator"], &[], &[]),
        ("blur", &["locator"], &[], &[]),
        ("scroll_to", &["locator"], &[], &[]),
        ("press_key", &["key"], &["modifiers"], &[]),
        ("insert_text", &["text"], &["locator"], &[]),
        (
            "press_sequentially",
            &["locator", "text"],
            &["delay_ms"],
            &[],
        ),
        (
            "drag_and_drop",
            &["source", "target"],
            &["source_position", "target_position"],
            &[],
        ),
        ("drop_files", &["paths", "target"], &[], &[]),
        ("mouse_move", &["x", "y"], &["steps"], &[]),
        ("mouse_down", &[], &["button"], &[]),
        ("mouse_up", &[], &["button"], &[]),
        (
            "mouse_click_xy",
            &["x", "y"],
            &["button", "click_count", "delay"],
            &[],
        ),
        (
            "mouse_drag_xy",
            &["end_x", "end_y", "start_x", "start_y"],
            &[],
            &[],
        ),
        ("mouse_wheel", &["delta_x", "delta_y"], &[], &[]),
        (
            "fill",
            &["locator", "text"],
            &["clear_first", "verify"],
            &[],
        ),
        ("clear", &["locator"], &["verify"], &[]),
        ("select_option", &["locator", "value"], &[], &[]),
        ("check", &["locator"], &[], &[]),
        ("uncheck", &["locator"], &[], &[]),
        ("set_input_files", &["locator", "paths"], &[], &[]),
        ("expect_file_chooser", &["paths"], &[], &[]),
        (
            "wait_for_function",
            &["expression"],
            &["locator", "polling_ms", "timeout_ms"],
            &[],
        ),
        (
            "wait_for_selector",
            &["locator"],
            &["state", "timeout_ms"],
            &[],
        ),
        ("wait_for_navigation", &[], &["timeout_ms"], &[]),
        ("wait_for_url", &["pattern"], &["timeout_ms"], &["contains"]),
        ("wait_for_text", &["text"], &["timeout_ms"], &[]),
        ("wait_for_network_idle", &[], &["timeout_ms"], &[]),
        ("wait_for_load_state", &["state"], &["timeout_ms"], &[]),
        (
            "wait_for_request",
            &["pattern"],
            &["method", "timeout_ms"],
            &["url_or_pattern"],
        ),
        (
            "wait_for_response",
            &["pattern"],
            &["method", "status", "timeout_ms"],
            &["url_or_pattern"],
        ),
        (
            "wait_for_console_message",
            &[],
            &["contains", "level", "timeout_ms"],
            &[],
        ),
        ("wait_for_download", &[], &["save_as", "timeout_ms"], &[]),
        ("cancel_download", &[], &["id"], &[]),
        (
            "wait_for_element_hidden",
            &["locator"],
            &["timeout_ms"],
            &[],
        ),
        (
            "wait_for_element_stable",
            &["locator"],
            &["timeout_ms"],
            &[],
        ),
        ("wait_seconds", &["seconds"], &[], &[]),
        ("get_text", &["locator"], &[], &[]),
        ("get_html", &["locator"], &[], &[]),
        ("get_attribute", &["attribute", "locator"], &[], &[]),
        ("bounding_box", &["locator"], &[], &[]),
        ("count", &["locator"], &[], &[]),
        ("input_value", &["locator"], &[], &[]),
        ("all_texts", &["locator"], &["limit", "mode"], &[]),
        ("element_state", &["locator"], &[], &[]),
        ("extract_links", &[], &["limit", "locator"], &[]),
        ("extract_table", &["locator"], &["limit"], &[]),
        ("dom_snapshot", &["selector"], &["max_chars"], &[]),
        (
            "accessibility_snapshot",
            &[],
            &["boxes", "depth", "locator", "max_chars", "mode", "refs"],
            &["root"],
        ),
        (
            "screenshot",
            &[],
            &[
                "animations",
                "caret",
                "clip",
                "full_page",
                "mask",
                "mask_color",
                "omit_background",
                "quality",
                "scale",
                "style",
                "type",
            ],
            &[],
        ),
        (
            "screenshot_element",
            &["locator"],
            &[
                "animations",
                "caret",
                "clip",
                "full_page",
                "mask",
                "mask_color",
                "omit_background",
                "quality",
                "scale",
                "style",
                "type",
            ],
            &[],
        ),
        (
            "screenshot_elements",
            &["locators"],
            &[
                "animations",
                "caret",
                "clip",
                "compose",
                "full_page",
                "labels",
                "mask",
                "mask_color",
                "omit_background",
                "quality",
                "scale",
                "style",
                "type",
            ],
            &[],
        ),
        (
            "capture_element_states",
            &["locator"],
            &[
                "animations",
                "caret",
                "clip",
                "full_page",
                "labels",
                "mask",
                "mask_color",
                "omit_background",
                "quality",
                "scale",
                "states",
                "style",
                "type",
            ],
            &[],
        ),
        (
            "pdf",
            &[],
            &[
                "format",
                "height",
                "landscape",
                "margins",
                "outline",
                "page_ranges",
                "prefer_css_page_size",
                "print_background",
                "scale",
                "tagged",
                "width",
            ],
            &[],
        ),
        ("eval", &["expression"], &["timeout_ms"], &[]),
        ("styles", &["locator"], &["property_filter"], &[]),
        ("set_content", &["html"], &["wait_until"], &[]),
        ("page_content", &[], &[], &[]),
        (
            "add_script_tag",
            &[],
            &["content", "script_type", "url"],
            &[],
        ),
        ("add_style_tag", &[], &["content", "url"], &[]),
        ("add_init_script", &["content"], &[], &[]),
        ("remove_init_script", &["id"], &[], &[]),
        (
            "dispatch_event",
            &["event_type", "locator"],
            &["event_init"],
            &[],
        ),
        ("tab_log", &[], &[], &[]),
        (
            "add_locator_handler",
            &["handler", "locator", "name"],
            &["no_wait_after", "times"],
            &[],
        ),
        ("remove_locator_handler", &["name"], &[], &[]),
        ("handle_dialog", &["accept"], &["prompt_text"], &[]),
        ("dismiss_overlays", &[], &["aggressive"], &[]),
        ("highlight_element", &["locator"], &[], &[]),
        ("highlight", &["locator"], &["label", "style"], &[]),
        ("hide_highlight", &[], &[], &[]),
        ("annotate", &["locator", "text"], &[], &[]),
        ("help", &[], &["topic"], &[]),
    ];
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    SharedDefault,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        SessionPolicy::SharedDefault
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkReportMode {
    None,
    #[default]
    Summary,
    Full,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PageContextMode {
    #[default]
    Snapshot,
    Screenshot,
    Both,
    None,
}

impl PageContextMode {
    pub fn includes_snapshot(self) -> bool {
        matches!(self, Self::Snapshot | Self::Both)
    }

    pub fn includes_screenshot(self) -> bool {
        matches!(self, Self::Screenshot | Self::Both)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserActionRequest {
    #[serde(default)]
    pub session: SessionPolicy,
    #[serde(default)]
    pub target: TabTarget,
    #[serde(default)]
    pub attach_screenshot: Option<bool>,
    #[serde(default)]
    pub page_context: Option<PageContextMode>,
    #[serde(default)]
    pub network: NetworkReportMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_service_workers: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_on_error: bool,
    pub steps: Vec<BrowserStep>,
}

impl BrowserActionRequest {
    pub fn page_context_mode(&self) -> PageContextMode {
        self.page_context.unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    TextInput,
    PasswordInput,
    EmailInput,
    SearchInput,
    NumberInput,
    TelInput,
    UrlInput,
    Textarea,
    Select,
    Checkbox,
    Radio,
    ContentEditable,
    DateInput,
    FileInput,
    HiddenInput,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FillStrategy {
    NativeTyping,
    DomValueSetter,
    NativePrototypeSetter,
    ContentEditablePath,
    ClickAndType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionabilityDiagnostics {
    pub call_log: Vec<String>,
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receives_events: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intercepting_element: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserAssertionResult {
    pub matcher: String,
    pub passed: bool,
    pub soft: bool,
    pub expected: serde_json::Value,
    pub received: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub attempts: u32,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_index: usize,
    pub ok: bool,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_kind: Option<FieldKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_strategy: Option<FillStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability: Option<ActionabilityDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion: Option<BrowserAssertionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator_echo: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

impl StepResult {
    pub fn success(step_index: usize, summary: impl Into<String>) -> Self {
        Self {
            step_index,
            ok: true,
            summary: summary.into(),
            error: None,
            data: None,
            field_kind: None,
            fill_strategy: None,
            verified: None,
            retries: 0,
            actionability: None,
            assertion: None,
            locator_echo: None,
            skipped: false,
        }
    }

    pub fn failure(
        step_index: usize,
        summary: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            step_index,
            ok: false,
            summary: summary.into(),
            error: Some(error.into()),
            data: None,
            field_kind: None,
            fill_strategy: None,
            verified: None,
            retries: 0,
            actionability: None,
            assertion: None,
            locator_echo: None,
            skipped: false,
        }
    }

    pub fn skipped(step_index: usize, failed_step_index: usize) -> Self {
        let mut result = Self::success(
            step_index,
            format!("Skipped: step {failed_step_index} failed earlier in this batch"),
        );
        result.ok = false;
        result.skipped = true;
        result
    }

    pub fn was_executed(&self) -> bool {
        !self.skipped
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn with_locator_echo(mut self, locator_echo: Option<String>) -> Self {
        self.locator_echo = locator_echo;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserConsoleCounts {
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub warnings: usize,
}

impl BrowserConsoleCounts {
    pub fn is_empty(&self) -> bool {
        self.errors == 0 && self.warnings == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserSnapshotArtifact {
    pub kind: String,
    pub mime: String,
    pub path: std::path::PathBuf,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserPageSnapshot {
    pub yaml: String,
    pub lines: usize,
    pub bytes: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<BrowserSnapshotArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<BrowserFrameSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserFrameSummary {
    pub frame_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub depth: usize,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserPageContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "BrowserConsoleCounts::is_empty")]
    pub console: BrowserConsoleCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<BrowserPageSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub ok: bool,
    pub steps: Vec<StepResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<BrowserPageContext>,
    #[serde(default)]
    pub stabilized: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console: Vec<ConsoleEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<NetworkEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_summary: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub websockets: Vec<WebSocketEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locator_handlers: Vec<LocatorHandlerFiring>,
    pub dialogs: Vec<DialogInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<UploadInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<DownloadInfo>,
    pub new_tabs: Vec<TabInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_routes: Vec<RouteInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intercepted_requests: Vec<RouteInterception>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<BrowserContextSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<BrowserScreenshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserContextSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub cookie_count: usize,
    #[serde(default)]
    pub local_storage_count: usize,
    #[serde(default)]
    pub session_storage_count: usize,
    #[serde(default)]
    pub offline: bool,
    #[serde(default)]
    pub http_credentials: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserStorageKind {
    Local,
    Session,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserStorageItem {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrowserCookieSameSite {
    Strict,
    Lax,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default = "default_cookie_path")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<BrowserCookieSameSite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

fn default_cookie_path() -> String {
    "/".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserStorageOrigin {
    pub origin: String,
    #[serde(default)]
    pub local_storage: Vec<BrowserStorageItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexed_db: Vec<BrowserIndexedDbDatabase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserIndexedDbDatabase {
    pub name: String,
    pub version: f64,
    #[serde(default)]
    pub stores: Vec<BrowserIndexedDbStore>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserIndexedDbStore {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<serde_json::Value>,
    #[serde(default)]
    pub auto_increment: bool,
    #[serde(default)]
    pub records: Vec<BrowserIndexedDbRecord>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserIndexedDbRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<serde_json::Value>,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPermissionState {
    #[default]
    Granted,
    Denied,
    Prompt,
}

impl BrowserPermissionState {
    pub fn is_granted(&self) -> bool {
        matches!(self, BrowserPermissionState::Granted)
    }

    pub fn label(&self) -> &'static str {
        match self {
            BrowserPermissionState::Granted => "granted",
            BrowserPermissionState::Denied => "denied",
            BrowserPermissionState::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserStorageState {
    #[serde(default)]
    pub cookies: Vec<BrowserCookie>,
    #[serde(default)]
    pub origins: Vec<BrowserStorageOrigin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteInfo {
    pub pattern: UrlPattern,
    pub handler: RouteHandler,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub har: Option<HarRouteInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times_remaining: Option<u32>,
    #[serde(default)]
    pub order: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarRouteInfo {
    pub entry_count: usize,
    pub not_found: HarNotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteInterception {
    pub url: String,
    pub method: String,
    pub pattern: UrlPattern,
    pub action: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub redirect_hop: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    InProgress,
    Completed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadInfo {
    pub guid: String,
    pub url: String,
    pub frame_id: String,
    pub suggested_filename: String,
    pub local_path: String,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub state: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadInfo {
    pub paths: Vec<String>,
    pub source: String,
    pub in_memory_payloads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocatorHandlerFiring {
    pub name: String,
    pub action: String,
    pub outcome: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DialogType {
    Alert,
    Confirm,
    Prompt,
    Beforeunload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DialogAction {
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DialogInfo {
    #[serde(rename = "type")]
    pub dialog_type: DialogType,
    pub message: String,
    pub default_value: String,
    pub action: DialogAction,
    pub automatic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserScreenshot {
    pub mime: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementInfo {
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aria_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub visible: bool,
    pub enabled: bool,
    pub readonly: bool,
    pub content_editable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<ElementBBox>,
    pub field_kind: FieldKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementBBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabOpener {
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabInfo {
    pub id: String,
    pub target_id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener: Option<TabOpener>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by_step: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_cookie_and_storage_state_serde_round_trip() {
        let state = BrowserStorageState {
            cookies: vec![BrowserCookie {
                name: "session".to_string(),
                value: "secret".to_string(),
                domain: "example.test".to_string(),
                path: "/".to_string(),
                expires: Some(1_900_000_000.0),
                http_only: true,
                secure: true,
                same_site: Some(BrowserCookieSameSite::Lax),
                url: None,
            }],
            origins: vec![BrowserStorageOrigin {
                origin: "https://example.test".to_string(),
                local_storage: vec![BrowserStorageItem {
                    name: "logged_in".to_string(),
                    value: "true".to_string(),
                }],
                indexed_db: Vec::new(),
            }],
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["origins"][0]["local_storage"][0]["name"], "logged_in");
        assert_eq!(
            serde_json::from_value::<BrowserStorageState>(json).unwrap(),
            state
        );
    }

    #[test]
    fn context_step_serde_round_trip_masks_nothing_in_typed_input() {
        let step = BrowserStep::SetHttpCredentials {
            username: "user".to_string(),
            password: "secret".to_string(),
        };
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["action"], "set_http_credentials");
        assert!(matches!(
            serde_json::from_value::<BrowserStep>(json).unwrap(),
            BrowserStep::SetHttpCredentials { .. }
        ));
    }

    fn all_browser_steps() -> Vec<BrowserStep> {
        let locator = || BrowserLocator::reference("e1");
        vec![
            BrowserStep::Navigate {
                url: "https://example.com".to_string(),
                timeout_ms: Some(1_000),
            },
            BrowserStep::Reload,
            BrowserStep::GoBack,
            BrowserStep::GoForward,
            BrowserStep::OpenTab {
                device: Some("desktop".to_string()),
                url: Some("https://example.com/new".to_string()),
            },
            BrowserStep::CloseTab { tab: None },
            BrowserStep::SwitchTab {
                tab: TabTarget::Active,
            },
            BrowserStep::ListTabs,
            BrowserStep::WaitForPopup {
                timeout_ms: Some(1_000),
            },
            BrowserStep::Route {
                pattern: UrlPattern::Text("**/api/**".to_string()),
                handler: RouteHandler::Abort {
                    reason: "blockedbyclient".to_string(),
                },
                times: Some(2),
            },
            BrowserStep::Unroute { pattern: None },
            BrowserStep::ListRoutes,
            BrowserStep::RouteWebSocket {
                pattern: UrlPattern::Text("wss://example.com/**".to_string()),
                mode: WebSocketRouteMode::Mock,
                on_page_message: WebSocketMessageAction::Forward,
                on_server_message: WebSocketMessageAction::Forward,
            },
            BrowserStep::UnrouteWebSocket { pattern: None },
            BrowserStep::CloseWebSocket {
                pattern: UrlPattern::Text("wss://example.com/**".to_string()),
                code: Some(1001),
                reason: Some("going away".to_string()),
            },
            BrowserStep::SendWebSocketMessage {
                pattern: UrlPattern::Text("wss://example.com/**".to_string()),
                text: "hello".to_string(),
            },
            BrowserStep::WaitForWebSocketFrame {
                pattern: None,
                timeout_ms: Some(1_000),
            },
            BrowserStep::StartHarRecording {
                path: Some("page.har".to_string()),
                mode: HarMode::Full,
                content: HarContentPolicy::Embed,
                url_filter: None,
                update: None,
            },
            BrowserStep::StopHarRecording,
            BrowserStep::RouteFromHar {
                path: "page.har".to_string(),
                url_filter: None,
                not_found: HarNotFound::Abort,
            },
            BrowserStep::Reset,
            BrowserStep::CdpSend {
                method: "Runtime.evaluate".to_string(),
                params: Some(serde_json::json!({"expression": "1 + 1"})),
                target: CdpTarget::Page,
            },
            BrowserStep::ClockInstall {
                time: Some(ClockTime::Text("2020-02-02T00:00:00Z".to_string())),
            },
            BrowserStep::ClockFastForward {
                ticks: ClockTicks::Human("01:00".to_string()),
            },
            BrowserStep::ClockPauseAt {
                time: ClockTime::UnixMillis(1_580_601_600_000),
            },
            BrowserStep::ClockResume,
            BrowserStep::ClockRunFor {
                ticks: ClockTicks::Millis(1000),
            },
            BrowserStep::ClockSetFixedTime {
                time: ClockTime::Text("2020-02-02".to_string()),
            },
            BrowserStep::ClockSetSystemTime {
                time: ClockTime::UnixMillis(1_580_601_600_000),
            },
            BrowserStep::HttpRequest {
                options: BrowserHttpRequest {
                    url: "https://example.com/api/session".to_string(),
                    ..Default::default()
                },
            },
            BrowserStep::StartCoverage {
                js: Some(true),
                css: Some(true),
                reset_on_navigation: Some(true),
            },
            BrowserStep::StopCoverage,
            BrowserStep::CaptureFrames {
                duration_ms: Some(1_000),
                frame_count: Some(8),
                interval_ms: None,
                locator: Some(locator()),
                full_page: Some(false),
            },
            BrowserStep::ScreencastStart {
                quality: Some(80),
                max_width: Some(1_280),
                max_height: Some(800),
            },
            BrowserStep::ScreencastStop {
                compose: Some(true),
            },
            BrowserStep::AddVirtualAuthenticator {
                id: (),
                protocol: Some(BrowserAuthenticatorProtocol::Ctap2),
                transport: Some(BrowserAuthenticatorTransport::Usb),
                has_resident_key: Some(true),
                has_user_verification: Some(true),
                is_user_verified: Some(true),
            },
            BrowserStep::RemoveVirtualAuthenticator {
                id: "authenticator".to_string(),
            },
            BrowserStep::ListCredentials {
                id: "authenticator".to_string(),
            },
            BrowserStep::AddCredential {
                id: "authenticator".to_string(),
                credential: BrowserWebAuthnCredential {
                    credential_id: "credential".to_string(),
                    is_resident_credential: true,
                    rp_id: Some("example.com".to_string()),
                    private_key: "private".to_string(),
                    user_handle: Some("user".to_string()),
                    sign_count: 0,
                    large_blob: None,
                    backup_eligibility: None,
                    backup_state: None,
                    user_name: None,
                    user_display_name: None,
                },
            },
            BrowserStep::ClearCredentials {
                id: "authenticator".to_string(),
            },
            BrowserStep::SetUserVerified {
                id: "authenticator".to_string(),
                verified: true,
            },
            BrowserStep::SetViewport {
                width: 390,
                height: 844,
                device_scale_factor: Some(3.0),
                is_mobile: Some(true),
                has_touch: Some(true),
            },
            BrowserStep::SetWindowBounds {
                x: Some(40),
                y: Some(60),
                width: Some(1280),
                height: Some(800),
            },
            BrowserStep::EmulateMedia {
                color_scheme: Some("dark".to_string()),
                reduced_motion: None,
                forced_colors: None,
                contrast: None,
                media: None,
            },
            BrowserStep::SetLocale {
                locale: "ja-JP".to_string(),
            },
            BrowserStep::SetTimezone {
                timezone: "Asia/Tokyo".to_string(),
            },
            BrowserStep::SetUserAgent {
                user_agent: "agent".to_string(),
                accept_language: None,
            },
            BrowserStep::SetGeolocation {
                latitude: 1.0,
                longitude: 2.0,
                accuracy: None,
            },
            BrowserStep::SetOffline { offline: false },
            BrowserStep::SetNetworkConditions {
                offline: Some(false),
                latency_ms: Some(150.0),
                download_kbps: Some(1600.0),
                upload_kbps: Some(750.0),
                preset: Some("slow-3g".to_string()),
            },
            BrowserStep::SetCpuThrottling { rate: 4.0 },
            BrowserStep::EmulateDevice {
                name: "iPhone 13".to_string(),
            },
            BrowserStep::ListDevices {
                filter: Some("pixel".to_string()),
            },
            BrowserStep::SetExtraHttpHeaders {
                headers: BTreeMap::new(),
            },
            BrowserStep::GetCookies { urls: None },
            BrowserStep::SetCookies {
                cookies: Vec::new(),
            },
            BrowserStep::ClearCookies {
                name: None,
                domain: None,
                path: None,
            },
            BrowserStep::GetStorage {
                kind: BrowserStorageKind::Local,
                origin: None,
            },
            BrowserStep::SetStorage {
                kind: BrowserStorageKind::Local,
                items: Vec::new(),
            },
            BrowserStep::ClearStorage {
                kind: BrowserStorageKind::Session,
            },
            BrowserStep::StorageState {
                save_as: Some("auth.json".to_string()),
                indexed_db: Some(true),
            },
            BrowserStep::SetStorageState {
                state: BrowserStorageState::default(),
                indexed_db: None,
            },
            BrowserStep::GrantPermissions {
                permissions: vec!["geolocation".to_string()],
                origin: None,
                state: BrowserPermissionState::Denied,
            },
            BrowserStep::ClearPermissions,
            BrowserStep::SetHttpCredentials {
                username: "user".to_string(),
                password: "secret".to_string(),
            },
            BrowserStep::Expect {
                locator: Some(locator()),
                matcher: BrowserExpectation::ToBeVisible,
                timeout_ms: Some(1_000),
                soft: false,
                not: None,
            },
            BrowserStep::ExpectPoll {
                expression: "window.__count".to_string(),
                expected: serde_json::json!(3),
                matcher: BrowserPollMatcher::Gt,
                timeout_ms: Some(1_000),
                soft: Some(true),
            },
            BrowserStep::Click {
                locator: locator(),
                timeout_ms: None,
            },
            BrowserStep::ClickIfExists { locator: locator() },
            BrowserStep::Tap {
                locator: Some(locator()),
                x: None,
                y: None,
            },
            BrowserStep::Hover {
                locator: locator(),
                timeout_ms: None,
            },
            BrowserStep::Focus { locator: locator() },
            BrowserStep::Blur { locator: locator() },
            BrowserStep::ScrollTo { locator: locator() },
            BrowserStep::PressKey {
                key: "Enter".to_string(),
                modifiers: vec!["Ctrl".to_string()],
            },
            BrowserStep::InsertText {
                locator: Some(locator()),
                text: "hello".to_string(),
            },
            BrowserStep::PressSequentially {
                locator: locator(),
                text: "hello".to_string(),
                delay_ms: Some(10),
            },
            BrowserStep::DragAndDrop {
                source: locator(),
                target: locator(),
                source_position: Some(BrowserPosition { x: 1.0, y: 2.0 }),
                target_position: Some(BrowserPosition { x: 3.0, y: 4.0 }),
            },
            BrowserStep::DropFiles {
                target: locator(),
                paths: vec!["/tmp/file".to_string()],
            },
            BrowserStep::MouseMove {
                x: 10.0,
                y: 20.0,
                steps: Some(2),
            },
            BrowserStep::MouseDown {
                button: BrowserMouseButton::Left,
            },
            BrowserStep::MouseUp {
                button: BrowserMouseButton::Left,
            },
            BrowserStep::MouseClickXy {
                x: 10.0,
                y: 20.0,
                button: BrowserMouseButton::Left,
                click_count: Some(2),
                delay: Some(10),
            },
            BrowserStep::MouseDragXy {
                start_x: 1.0,
                start_y: 2.0,
                end_x: 3.0,
                end_y: 4.0,
            },
            BrowserStep::MouseWheel {
                delta_x: 0.0,
                delta_y: 100.0,
            },
            BrowserStep::Fill {
                locator: locator(),
                text: "hi".to_string(),
                clear_first: true,
                verify: true,
            },
            BrowserStep::Clear {
                locator: locator(),
                verify: true,
            },
            BrowserStep::SelectOption {
                locator: locator(),
                value: "one".to_string(),
            },
            BrowserStep::Check { locator: locator() },
            BrowserStep::Uncheck { locator: locator() },
            BrowserStep::SetInputFiles {
                locator: locator(),
                paths: vec!["/tmp/file".to_string()],
            },
            BrowserStep::ExpectFileChooser {
                paths: vec!["/tmp/file".to_string()],
            },
            BrowserStep::WaitForFunction {
                expression: "() => window.__ready".to_string(),
                locator: Some(locator()),
                timeout_ms: Some(1_000),
                polling_ms: Some(50),
            },
            BrowserStep::WaitForSelector {
                locator: locator(),
                state: Some(BrowserWaitState::Visible),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForNavigation {
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForUrl {
                pattern: UrlPattern::Text("/done".to_string()),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForText {
                text: "done".to_string(),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForNetworkIdle {
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForLoadState {
                state: BrowserLoadState::Load,
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForRequest {
                pattern: UrlPattern::Text("/api".to_string()),
                method: Some("GET".to_string()),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForResponse {
                pattern: UrlPattern::Regex {
                    source: "/api".to_string(),
                    flags: "i".to_string(),
                },
                method: Some("POST".to_string()),
                status: Some(200),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForConsoleMessage {
                contains: Some("ready".to_string()),
                level: Some(BrowserConsoleLevel::Error),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForDownload {
                timeout_ms: Some(1_000),
                save_as: Some("file.txt".to_string()),
            },
            BrowserStep::CancelDownload { id: None },
            BrowserStep::WaitForElementHidden {
                locator: locator(),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitForElementStable {
                locator: locator(),
                timeout_ms: Some(1_000),
            },
            BrowserStep::WaitSeconds { seconds: 0.1 },
            BrowserStep::GetText { locator: locator() },
            BrowserStep::GetHtml { locator: locator() },
            BrowserStep::GetAttribute {
                locator: locator(),
                attribute: "href".to_string(),
            },
            BrowserStep::BoundingBox { locator: locator() },
            BrowserStep::Count { locator: locator() },
            BrowserStep::InputValue { locator: locator() },
            BrowserStep::AllTexts {
                locator: locator(),
                mode: BrowserTextMode::TextContent,
                limit: Some(10),
            },
            BrowserStep::ElementState { locator: locator() },
            BrowserStep::ExtractLinks {
                locator: Some(locator()),
                limit: Some(10),
            },
            BrowserStep::ExtractTable {
                locator: locator(),
                limit: Some(10),
            },
            BrowserStep::DomSnapshot {
                selector: "main".to_string(),
                max_chars: Some(1_000),
            },
            BrowserStep::AccessibilitySnapshot {
                options: AccessibilitySnapshotOptions::default(),
            },
            BrowserStep::Screenshot {
                options: BrowserScreenshotOptions::default(),
            },
            BrowserStep::ScreenshotElement {
                locator: locator(),
                options: BrowserScreenshotOptions::default(),
            },
            BrowserStep::ScreenshotElements {
                locators: vec![locator()],
                compose: BrowserComposeMode::Grid,
                labels: Some(true),
                options: BrowserScreenshotOptions::default(),
            },
            BrowserStep::CaptureElementStates {
                locator: locator(),
                states: vec![BrowserElementState::Default, BrowserElementState::Hover],
                labels: Some(true),
                options: BrowserScreenshotOptions::default(),
            },
            BrowserStep::Pdf {
                options: BrowserPdfOptions::default(),
            },
            BrowserStep::Eval {
                expression: "document.title".to_string(),
                timeout_ms: None,
            },
            BrowserStep::Styles {
                locator: locator(),
                property_filter: Some("color".to_string()),
            },
            BrowserStep::SetContent {
                html: "<p>hi</p>".to_string(),
                wait_until: Some(BrowserLoadState::Load),
            },
            BrowserStep::PageContent,
            BrowserStep::AddScriptTag {
                url: None,
                content: Some("window.ok = 1".to_string()),
                script_type: Some("module".to_string()),
            },
            BrowserStep::AddStyleTag {
                url: Some("https://example.com/app.css".to_string()),
                content: None,
            },
            BrowserStep::AddInitScript {
                content: "window.__flag = 1".to_string(),
            },
            BrowserStep::RemoveInitScript {
                id: "init1".to_string(),
            },
            BrowserStep::DispatchEvent {
                locator: locator(),
                event_type: "click".to_string(),
                event_init: Some(serde_json::json!({"detail": 2})),
            },
            BrowserStep::TabLog,
            BrowserStep::AddLocatorHandler {
                name: "overlay".to_string(),
                locator: locator(),
                handler: LocatorHandlerAction::Click,
                times: Some(1),
                no_wait_after: false,
            },
            BrowserStep::RemoveLocatorHandler {
                name: "overlay".to_string(),
            },
            BrowserStep::HandleDialog {
                accept: true,
                prompt_text: Some("answer".to_string()),
            },
            BrowserStep::DismissOverlays { aggressive: true },
            BrowserStep::HighlightElement { locator: locator() },
            BrowserStep::Highlight {
                locator: locator(),
                style: Some("outline: 2px solid red".to_string()),
                label: Some("Target".to_string()),
            },
            BrowserStep::HideHighlight,
            BrowserStep::Annotate {
                locator: locator(),
                text: "Review".to_string(),
            },
            BrowserStep::Help {
                topic: Some("routes".to_string()),
            },
        ]
    }

    #[test]
    fn help_topic_is_optional_and_omitted_from_the_wire_when_unset() {
        let bare: BrowserStep =
            serde_json::from_value(serde_json::json!({"action": "help"})).unwrap();
        assert!(matches!(bare, BrowserStep::Help { topic: None }));
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({"action": "help"})
        );

        let scoped: BrowserStep =
            serde_json::from_value(serde_json::json!({"action": "help", "topic": "locators"}))
                .unwrap();
        assert!(matches!(
            scoped,
            BrowserStep::Help { topic: Some(ref topic) } if topic == "locators"
        ));
    }

    #[test]
    fn add_virtual_authenticator_parses_without_an_id_and_never_serializes_one() {
        let step: BrowserStep = serde_json::from_str(
            r#"{"action": "add_virtual_authenticator", "protocol": "ctap2", "transport": "internal"}"#,
        )
        .unwrap();

        assert!(matches!(
            step,
            BrowserStep::AddVirtualAuthenticator {
                protocol: Some(BrowserAuthenticatorProtocol::Ctap2),
                transport: Some(BrowserAuthenticatorTransport::Internal),
                ..
            }
        ));
        assert!(
            serde_json::to_value(&step).unwrap().get("id").is_none(),
            "add_virtual_authenticator must not carry an id on the wire"
        );
    }

    #[test]
    fn add_virtual_authenticator_rejects_a_client_supplied_id() {
        let error = serde_json::from_str::<BrowserStep>(
            r#"{"action": "add_virtual_authenticator", "id": "mine"}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("id is server-minted"), "{error}");
        assert!(error.contains("use the returned id"), "{error}");
    }

    #[test]
    fn other_webauthn_steps_still_address_the_minted_id() {
        let step: BrowserStep = serde_json::from_str(
            r#"{"action": "remove_virtual_authenticator", "id": "minted-uuid"}"#,
        )
        .unwrap();

        assert!(matches!(
            step,
            BrowserStep::RemoveVirtualAuthenticator { ref id } if id == "minted-uuid"
        ));
    }

    #[test]
    fn route_web_socket_defaults_to_mock_with_both_directions_forwarding() {
        let step: BrowserStep = serde_json::from_value(
            serde_json::json!({"action": "route_web_socket", "pattern": "wss://example.com/**"}),
        )
        .unwrap();

        assert!(matches!(
            step,
            BrowserStep::RouteWebSocket {
                mode: WebSocketRouteMode::Mock,
                on_page_message: WebSocketMessageAction::Forward,
                on_server_message: WebSocketMessageAction::Forward,
                ..
            }
        ));
    }

    #[test]
    fn route_web_socket_accepts_intercept_mode_with_per_direction_overrides() {
        let step: BrowserStep = serde_json::from_value(serde_json::json!({
            "action": "route_web_socket",
            "pattern": "wss://example.com/**",
            "mode": "intercept",
            "on_page_message": "drop",
            "on_server_message": "capture",
        }))
        .unwrap();

        assert!(matches!(
            step,
            BrowserStep::RouteWebSocket {
                mode: WebSocketRouteMode::Intercept,
                on_page_message: WebSocketMessageAction::Drop,
                on_server_message: WebSocketMessageAction::Capture,
                ..
            }
        ));
    }

    #[test]
    fn close_web_socket_takes_an_optional_code_and_reason() {
        let bare: BrowserStep = serde_json::from_value(
            serde_json::json!({"action": "close_web_socket", "pattern": "wss://example.com/**"}),
        )
        .unwrap();
        assert!(matches!(
            bare,
            BrowserStep::CloseWebSocket {
                code: None,
                reason: None,
                ..
            }
        ));

        let detailed: BrowserStep = serde_json::from_value(serde_json::json!({
            "action": "close_web_socket",
            "pattern": "wss://example.com/**",
            "code": 1001,
            "reason": "going away",
        }))
        .unwrap();
        assert!(matches!(
            detailed,
            BrowserStep::CloseWebSocket {
                code: Some(1001),
                ref reason,
                ..
            } if reason.as_deref() == Some("going away")
        ));
    }

    #[test]
    fn browser_step_action_names_cover_every_variant() {
        let serialized = all_browser_steps()
            .into_iter()
            .map(|step| {
                serde_json::to_value(step).unwrap()["action"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect::<std::collections::BTreeSet<_>>();
        let declared = BrowserStep::ACTION_NAMES
            .iter()
            .map(|action| action.to_string())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(declared, serialized);
    }

    #[test]
    fn clock_ticks_accept_milliseconds_and_human_strings() {
        let millis: BrowserStep =
            serde_json::from_value(serde_json::json!({"action": "clock_run_for", "ticks": 1000}))
                .unwrap();
        assert!(matches!(
            millis,
            BrowserStep::ClockRunFor {
                ticks: ClockTicks::Millis(1000)
            }
        ));
        let human: BrowserStep = serde_json::from_value(
            serde_json::json!({"action": "clock_fast_forward", "ticks": "01:00"}),
        )
        .unwrap();
        let BrowserStep::ClockFastForward { ticks } = &human else {
            panic!("expected clock_fast_forward, got {human:?}");
        };
        assert_eq!(*ticks, ClockTicks::Human("01:00".to_string()));
        assert_eq!(
            serde_json::to_value(&human).unwrap(),
            serde_json::json!({"action": "clock_fast_forward", "ticks": "01:00"})
        );
    }

    #[test]
    fn clock_time_accepts_unix_millis_and_iso_strings() {
        let numeric: BrowserStep = serde_json::from_value(
            serde_json::json!({"action": "clock_pause_at", "time": 1_580_601_600_000i64}),
        )
        .unwrap();
        let BrowserStep::ClockPauseAt { time } = &numeric else {
            panic!("expected clock_pause_at, got {numeric:?}");
        };
        assert_eq!(*time, ClockTime::UnixMillis(1_580_601_600_000));
        let iso: BrowserStep = serde_json::from_value(
            serde_json::json!({"action": "clock_set_fixed_time", "time": "2020-02-02"}),
        )
        .unwrap();
        let BrowserStep::ClockSetFixedTime { time } = &iso else {
            panic!("expected clock_set_fixed_time, got {iso:?}");
        };
        assert_eq!(*time, ClockTime::Text("2020-02-02".to_string()));
    }

    #[test]
    fn clock_install_time_is_optional_and_resume_takes_no_parameters() {
        let install: BrowserStep =
            serde_json::from_value(serde_json::json!({"action": "clock_install"})).unwrap();
        assert!(matches!(install, BrowserStep::ClockInstall { time: None }));
        assert_eq!(
            serde_json::to_value(&install).unwrap(),
            serde_json::json!({"action": "clock_install"})
        );
        let resume: BrowserStep =
            serde_json::from_value(serde_json::json!({"action": "clock_resume"})).unwrap();
        assert!(matches!(resume, BrowserStep::ClockResume));
    }

    #[test]
    fn clock_steps_reject_missing_required_parameters() {
        for action in [
            "clock_fast_forward",
            "clock_run_for",
            "clock_pause_at",
            "clock_set_fixed_time",
            "clock_set_system_time",
        ] {
            assert!(
                serde_json::from_value::<BrowserStep>(serde_json::json!({"action": action}))
                    .is_err(),
                "{action} accepted a missing parameter"
            );
        }
    }

    #[test]
    fn test_locator_css_serde() {
        let loc = BrowserLocator::css("#btn");
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "css");
        assert_eq!(json["value"], "#btn");
        let parsed: BrowserLocator = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, loc);
    }

    #[test]
    fn ref_locator_round_trips() {
        for reference in ["e12", "f2e7"] {
            let locator = BrowserLocator::reference(reference);
            let value = serde_json::to_value(&locator).unwrap();
            assert_eq!(value, serde_json::json!({"by": "ref", "value": reference}));
            assert_eq!(
                serde_json::from_value::<BrowserLocator>(value).unwrap(),
                locator
            );
        }
    }

    #[test]
    fn frame_chain_round_trips_outermost_first() {
        let locator = BrowserLocator::role("button", Some("Save")).in_frames(vec![
            BrowserLocator::css("#outer"),
            BrowserLocator::role("iframe", Some("Editor")),
        ]);
        let value = serde_json::to_value(&locator).unwrap();

        assert_eq!(
            value["frames"],
            serde_json::json!([
                {"by": "css", "value": "#outer"},
                {"by": "role", "role": "iframe", "name": "Editor"}
            ])
        );
        assert_eq!(
            serde_json::from_value::<BrowserLocator>(value).unwrap(),
            locator
        );
    }

    #[test]
    fn test_locator_id_serde() {
        let loc = BrowserLocator::id("email");
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "id");
        assert_eq!(json["value"], "email");
    }

    #[test]
    fn test_locator_name_serde() {
        let loc = BrowserLocator::name("q");
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "name");
        assert_eq!(json["value"], "q");
    }

    #[test]
    fn test_locator_label_serde() {
        let loc = BrowserLocator::label("Email Address");
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "label");
        assert_eq!(json["value"], "Email Address");
    }

    #[test]
    fn test_locator_placeholder_serde() {
        let loc = BrowserLocator::placeholder("Search...");
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "placeholder");
        assert_eq!(json["value"], "Search...");
    }

    #[test]
    fn test_locator_role_serde() {
        let loc = BrowserLocator::role("textbox", Some("Search"));
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "role");
        assert_eq!(json["role"], "textbox");
        assert_eq!(json["name"], "Search");
    }

    #[test]
    fn test_locator_role_without_name_serde() {
        let loc = BrowserLocator::role("button", None);
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "role");
        assert_eq!(json["role"], "button");
        assert!(json.get("name").is_none());
    }

    #[test]
    fn test_locator_test_id_serde() {
        let loc = BrowserLocator::test_id("submit-btn");
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["by"], "test_id");
        assert_eq!(json["value"], "submit-btn");
    }

    #[test]
    fn test_locator_text_serde() {
        let json_str = r#"{"by": "text", "value": "Submit Form", "exact": true}"#;
        let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
        match &loc.strategy {
            LocatorStrategy::Text { value, exact, .. } => {
                assert_eq!(value, "Submit Form");
                assert!(*exact);
            }
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn old_locator_wire_shapes_still_round_trip() {
        for json in [
            r#"{"by":"test_id","value":"save"}"#,
            r#"{"by":"placeholder","value":"Search"}"#,
            r#"{"by":"text","value":"Submit"}"#,
            r#"{"by":"label","value":"Email"}"#,
            r#"{"by":"role","role":"button","name":"Save"}"#,
        ] {
            let locator: BrowserLocator = serde_json::from_str(json).unwrap();
            let round_trip = serde_json::to_value(&locator).unwrap();
            let expected: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(round_trip, expected);
        }
    }

    #[test]
    fn get_by_locator_options_round_trip() {
        let json = serde_json::json!({
            "by": "role",
            "role": "checkbox",
            "name_regex": {"source": "save\\s+item", "flags": "i"},
            "description": "primary action",
            "description_regex": {"source": "primary", "flags": "u"},
            "exact": true,
            "checked": "mixed",
            "pressed": false,
            "selected": true,
            "expanded": false,
            "disabled": true,
            "level": 3,
            "include_hidden": true,
            "nth": 1,
            "within": "#dialog"
        });
        let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(locator).unwrap(), json);
    }

    #[test]
    fn attribute_and_regex_locator_options_round_trip() {
        for json in [
            serde_json::json!({
                "by": "test_id",
                "value": "ignored for regex",
                "regex": {"source": "save-\\d+", "flags": "i"},
                "exact": true,
                "attribute": "data-qa"
            }),
            serde_json::json!({"by": "alt_text", "value": "logo", "exact": false}),
            serde_json::json!({"by": "title", "value": "Help", "exact": true}),
        ] {
            let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(locator).unwrap(), json);
        }
    }

    #[test]
    fn test_locator_xpath_serde() {
        let json_str = r#"{"by": "xpath", "value": "//button[@type='submit']"}"#;
        let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
        match &loc.strategy {
            LocatorStrategy::Xpath { value } => {
                assert!(value.contains("submit"));
            }
            _ => panic!("Expected Xpath"),
        }
    }

    #[test]
    fn test_locator_autocomplete_serde() {
        let json_str = r#"{"by": "autocomplete", "value": "email"}"#;
        let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
        match &loc.strategy {
            LocatorStrategy::Autocomplete { value } => {
                assert_eq!(value, "email");
            }
            _ => panic!("Expected Autocomplete"),
        }
    }

    #[test]
    fn test_locator_with_nth_and_within() {
        let json_str = r##"{"by": "css", "value": "input", "nth": 2, "within": "#form"}"##;
        let loc: BrowserLocator = serde_json::from_str(json_str).unwrap();
        assert_eq!(loc.nth, Some(2));
        assert_eq!(loc.within.as_deref(), Some("#form"));
    }

    #[test]
    fn test_locator_nth_and_within_omitted_when_none() {
        let loc = BrowserLocator::css("div");
        let json = serde_json::to_value(&loc).unwrap();
        assert!(json.get("nth").is_none());
        assert!(json.get("within").is_none());
    }

    #[test]
    fn composed_locator_wire_round_trips() {
        let json = serde_json::json!({
            "by": "css",
            "value": ".card",
            "locator": {"by": "role", "role": "button", "name": "Open"},
            "filter": {
                "has": {"by": "css", "value": ".ready"},
                "has_not": {"by": "css", "value": ".disabled"},
                "has_text": "Alpha",
                "has_not_text": {"source": "archived", "flags": "i"},
                "visible": true
            },
            "and": {"by": "css", "value": ".featured"},
            "or": {"by": "test_id", "value": "fallback"},
            "last": true
        });
        let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(locator).unwrap(), json);
    }

    #[test]
    fn first_last_and_signed_nth_round_trip() {
        for json in [
            serde_json::json!({"by":"css","value":"li","first":true}),
            serde_json::json!({"by":"css","value":"li","last":true}),
            serde_json::json!({"by":"css","value":"li","nth":-1}),
        ] {
            let locator: BrowserLocator = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(locator).unwrap(), json);
        }
    }

    #[test]
    fn test_tab_target_active_serde() {
        let t = TabTarget::Active;
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["type"], "active");
    }

    #[test]
    fn test_tab_target_id_serde() {
        let t = TabTarget::Id {
            id: "main".to_string(),
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["type"], "id");
        assert_eq!(json["id"], "main");
    }

    #[test]
    fn test_step_navigate_serde() {
        let step = BrowserStep::Navigate {
            url: "https://example.com".to_string(),
            timeout_ms: None,
        };
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["action"], "navigate");
        assert_eq!(json["url"], "https://example.com");
    }

    #[test]
    fn test_step_click_serde() {
        let json_str = r##"{"action": "click", "locator": {"by": "css", "value": "#btn"}}"##;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::Click { locator, .. } => {
                assert_eq!(
                    locator.strategy,
                    LocatorStrategy::Css {
                        value: "#btn".to_string()
                    }
                );
            }
            _ => panic!("Expected Click"),
        }
    }

    #[test]
    fn test_step_fill_serde() {
        let json_str = r#"{
            "action": "fill",
            "locator": {"by": "name", "value": "q"},
            "text": "rust tutorial",
            "clear_first": true,
            "verify": true
        }"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::Fill {
                locator,
                text,
                clear_first,
                verify,
            } => {
                assert_eq!(text, "rust tutorial");
                assert!(clear_first);
                assert!(verify);
                match &locator.strategy {
                    LocatorStrategy::Name { value } => assert_eq!(value, "q"),
                    _ => panic!("Expected Name locator"),
                }
            }
            _ => panic!("Expected Fill"),
        }
    }

    #[test]
    fn test_step_fill_defaults() {
        let json_str = r##"{
            "action": "fill",
            "locator": {"by": "css", "value": "#input"},
            "text": "hello"
        }"##;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::Fill {
                clear_first,
                verify,
                ..
            } => {
                assert!(clear_first, "clear_first should default to true");
                assert!(verify, "verify should default to true");
            }
            _ => panic!("Expected Fill"),
        }
    }

    #[test]
    fn test_step_wait_for_url_serde() {
        let json_str = r#"{"action": "wait_for_url", "pattern": "/search", "timeout_ms": 5000}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::WaitForUrl {
                pattern,
                timeout_ms,
            } => {
                assert_eq!(pattern, UrlPattern::Text("/search".to_string()));
                assert_eq!(timeout_ms, Some(5000));
            }
            _ => panic!("Expected WaitForUrl"),
        }

        let regex: BrowserStep = serde_json::from_str(
            r#"{"action": "wait_for_url", "pattern": {"source": "/search\\?q=", "flags": "i"}}"#,
        )
        .unwrap();
        assert!(matches!(
            regex,
            BrowserStep::WaitForUrl {
                pattern: UrlPattern::Regex { .. },
                ..
            }
        ));
    }

    #[test]
    fn test_network_wait_steps_serde() {
        let load: BrowserStep = serde_json::from_str(
            r#"{"action":"wait_for_load_state","state":"networkidle","timeout_ms":7000}"#,
        )
        .unwrap();
        assert!(matches!(
            load,
            BrowserStep::WaitForLoadState {
                state: BrowserLoadState::Networkidle,
                timeout_ms: Some(7000)
            }
        ));

        let response: BrowserStep = serde_json::from_str(
            r#"{"action":"wait_for_response","pattern":{"source":"/api/.*","flags":"i"}}"#,
        )
        .unwrap();
        assert!(matches!(
            response,
            BrowserStep::WaitForResponse {
                pattern: UrlPattern::Regex { source, flags },
                method: None,
                status: None,
                timeout_ms: None
            } if source == "/api/.*" && flags == "i"
        ));

        let request: BrowserStep =
            serde_json::from_str(r#"{"action":"wait_for_request","pattern":{"source":"/api/.*"}}"#)
                .unwrap();
        assert!(matches!(
            request,
            BrowserStep::WaitForRequest {
                pattern: UrlPattern::Regex { source, flags },
                method: None,
                timeout_ms: None
            } if source == "/api/.*" && flags.is_empty()
        ));
    }

    #[test]
    fn route_handlers_and_patternless_unroute_round_trip() {
        for value in [
            serde_json::json!({
                "action": "route",
                "pattern": "**/api/**",
                "handler": {
                    "type": "fulfill",
                    "status": 200,
                    "content_type": "application/json",
                    "body": "{\"ok\":true}",
                    "headers": {"X-Mocked": "yes"}
                }
            }),
            serde_json::json!({
                "action": "route",
                "pattern": "**/*.png",
                "handler": {"type": "abort", "reason": "blockedbyclient"}
            }),
            serde_json::json!({
                "action": "route",
                "pattern": {"source": "/api/", "flags": "i"},
                "handler": {
                    "type": "continue",
                    "url": "https://example.com/api/other",
                    "method": "POST",
                    "headers": {"X-Test": "route"},
                    "post_data": "password=secret"
                }
            }),
            serde_json::json!({"action": "unroute"}),
        ] {
            let step: BrowserStep = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(step).unwrap(), value);
        }
    }

    #[test]
    fn test_file_transfer_steps_serde() {
        let upload: BrowserStep = serde_json::from_str(
            r##"{"action":"set_input_files","locator":{"by":"css","value":"#file"},"paths":["/workspace/a.txt"]}"##,
        )
        .unwrap();
        assert!(matches!(
            upload,
            BrowserStep::SetInputFiles { paths, .. } if paths == vec!["/workspace/a.txt"]
        ));

        let chooser: BrowserStep = serde_json::from_str(
            r#"{"action":"expect_file_chooser","paths":["/workspace/a.txt"]}"#,
        )
        .unwrap();
        assert!(matches!(chooser, BrowserStep::ExpectFileChooser { paths } if paths.len() == 1));

        let download: BrowserStep = serde_json::from_str(
            r#"{"action":"wait_for_download","timeout_ms":9000,"save_as":"saved.txt"}"#,
        )
        .unwrap();
        assert!(matches!(
            download,
            BrowserStep::WaitForDownload {
                timeout_ms: Some(9000),
                save_as: Some(name)
            } if name == "saved.txt"
        ));
    }

    #[test]
    fn readout_steps_address_a_single_locator() {
        for (json, expected) in [
            (
                r#"{"action":"bounding_box","locator":{"by":"ref","value":"e1"}}"#,
                "bounding_box",
            ),
            (
                r#"{"action":"count","locator":{"by":"css","value":".row"}}"#,
                "count",
            ),
            (
                r#"{"action":"input_value","locator":{"by":"ref","value":"e1"}}"#,
                "input_value",
            ),
            (
                r#"{"action":"element_state","locator":{"by":"ref","value":"e1"}}"#,
                "element_state",
            ),
        ] {
            let step: BrowserStep = serde_json::from_str(json).unwrap();
            assert_eq!(serde_json::to_value(&step).unwrap()["action"], expected);
        }
    }

    #[test]
    fn all_texts_mode_defaults_to_inner_text_and_limit_is_optional() {
        let step: BrowserStep =
            serde_json::from_str(r#"{"action":"all_texts","locator":{"by":"css","value":"li"}}"#)
                .unwrap();

        assert!(matches!(
            step,
            BrowserStep::AllTexts {
                mode: BrowserTextMode::InnerText,
                limit: None,
                ..
            }
        ));

        let explicit: BrowserStep = serde_json::from_str(
            r#"{"action":"all_texts","locator":{"by":"css","value":"li"},"mode":"text_content","limit":5}"#,
        )
        .unwrap();

        assert!(matches!(
            explicit,
            BrowserStep::AllTexts {
                mode: BrowserTextMode::TextContent,
                limit: Some(5),
                ..
            }
        ));
    }

    #[test]
    fn readout_steps_reject_unknown_fields() {
        let error = serde_json::from_str::<BrowserStep>(
            r#"{"action":"all_texts","locator":{"by":"css","value":"li"},"limits":5}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("limits"), "{error}");
    }

    #[test]
    fn test_step_extract_links_serde() {
        let json_str = r#"{"action": "extract_links", "limit": 10}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::ExtractLinks { locator, limit } => {
                assert!(locator.is_none());
                assert_eq!(limit, Some(10));
            }
            _ => panic!("Expected ExtractLinks"),
        }
    }

    #[test]
    fn test_step_extract_table_serde() {
        let json_str = r#"{"action": "extract_table", "locator": {"by": "css", "value": "table"}, "limit": 4}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::ExtractTable { limit, .. } => assert_eq!(limit, Some(4)),
            _ => panic!("Expected ExtractTable"),
        }
    }

    #[test]
    fn test_step_extract_table_limit_defaults_to_none() {
        let json_str = r#"{"action": "extract_table", "locator": {"by": "css", "value": "table"}}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::ExtractTable { limit, .. } => assert_eq!(limit, None),
            _ => panic!("Expected ExtractTable"),
        }
    }

    #[test]
    fn test_step_screenshot_elements_serde() {
        let json_str = r##"{"action": "screenshot_elements", "locators": [{"by": "css", "value": ".card"}, {"by": "ref", "value": "e7"}], "compose": "separate", "labels": false, "type": "webp", "mask_color": "#00FF00"}"##;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::ScreenshotElements {
                locators,
                compose,
                labels,
                options,
            } => {
                assert_eq!(locators.len(), 2);
                assert_eq!(compose, BrowserComposeMode::Separate);
                assert_eq!(labels, Some(false));
                assert_eq!(options.image_type, Some(BrowserScreenshotType::Webp));
                assert_eq!(options.mask_color.as_deref(), Some("#00FF00"));
            }
            _ => panic!("Expected ScreenshotElements"),
        }
    }

    #[test]
    fn test_step_screenshot_elements_defaults_to_a_labelled_grid() {
        let json_str =
            r#"{"action": "screenshot_elements", "locators": [{"by": "css", "value": ".card"}]}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::ScreenshotElements {
                compose,
                labels,
                options,
                ..
            } => {
                assert_eq!(compose, BrowserComposeMode::Grid);
                assert_eq!(labels, None);
                assert_eq!(options, BrowserScreenshotOptions::default());
            }
            _ => panic!("Expected ScreenshotElements"),
        }
    }

    #[test]
    fn test_step_capture_element_states_serde() {
        let json_str = r#"{"action": "capture_element_states", "locator": {"by": "css", "value": "button"}, "states": ["default", "hover", "focus", "active"], "full_page": false}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::CaptureElementStates {
                states,
                labels,
                options,
                ..
            } => {
                assert_eq!(
                    states,
                    vec![
                        BrowserElementState::Default,
                        BrowserElementState::Hover,
                        BrowserElementState::Focus,
                        BrowserElementState::Active,
                    ]
                );
                assert_eq!(labels, None);
                assert!(!options.full_page);
            }
            _ => panic!("Expected CaptureElementStates"),
        }
    }

    #[test]
    fn test_element_states_round_trip_on_the_wire() {
        for (state, wire) in [
            (BrowserElementState::Default, "default"),
            (BrowserElementState::Hover, "hover"),
            (BrowserElementState::Focus, "focus"),
            (BrowserElementState::Active, "active"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), wire);
            assert_eq!(state.label(), wire);
            assert_eq!(
                serde_json::from_value::<BrowserElementState>(serde_json::json!(wire)).unwrap(),
                state
            );
        }
        assert!(
            serde_json::from_value::<BrowserElementState>(serde_json::json!("visited")).is_err()
        );
    }

    #[test]
    fn wait_for_function_defaults_to_a_page_predicate_without_a_fixed_interval() {
        let step: BrowserStep =
            serde_json::from_str(r#"{"action": "wait_for_function", "expression": "() => ready"}"#)
                .unwrap();

        let BrowserStep::WaitForFunction {
            expression,
            locator,
            timeout_ms,
            polling_ms,
        } = step
        else {
            panic!("expected WaitForFunction");
        };
        assert_eq!(expression, "() => ready");
        assert!(locator.is_none());
        assert!(timeout_ms.is_none());
        assert!(polling_ms.is_none());
    }

    #[test]
    fn wait_for_function_accepts_a_locator_and_a_fixed_interval() {
        let step: BrowserStep = serde_json::from_str(
            r##"{"action": "wait_for_function", "expression": "el => el.dataset.state === 'ready'", "locator": {"by": "css", "value": "#row"}, "timeout_ms": 2000, "polling_ms": 40}"##,
        )
        .unwrap();

        let BrowserStep::WaitForFunction {
            locator,
            timeout_ms,
            polling_ms,
            ..
        } = step
        else {
            panic!("expected WaitForFunction");
        };
        assert!(locator.is_some());
        assert_eq!(timeout_ms, Some(2_000));
        assert_eq!(polling_ms, Some(40));
    }

    #[test]
    fn expect_poll_round_trips_every_matcher_name() {
        for (name, expected) in [
            ("equals", BrowserPollMatcher::Equals),
            ("contains", BrowserPollMatcher::Contains),
            ("gt", BrowserPollMatcher::Gt),
            ("lt", BrowserPollMatcher::Lt),
            ("matches_regex", BrowserPollMatcher::MatchesRegex),
        ] {
            let step: BrowserStep = serde_json::from_value(serde_json::json!({
                "action": "expect_poll",
                "expression": "window.__count",
                "expected": 3,
                "matcher": name,
            }))
            .unwrap();

            let BrowserStep::ExpectPoll { matcher, soft, .. } = step else {
                panic!("expected ExpectPoll");
            };
            assert_eq!(matcher, expected);
            assert_eq!(matcher.name(), name);
            assert!(soft.is_none());
            assert_eq!(
                serde_json::to_value(matcher).unwrap(),
                serde_json::json!(name)
            );
        }
    }

    #[test]
    fn expect_poll_requires_an_expression_expected_value_and_matcher() {
        for missing in [
            serde_json::json!({"action": "expect_poll", "expected": 1, "matcher": "equals"}),
            serde_json::json!({"action": "expect_poll", "expression": "x", "matcher": "equals"}),
            serde_json::json!({"action": "expect_poll", "expression": "x", "expected": 1}),
            serde_json::json!({"action": "expect_poll", "expression": "x", "expected": 1, "matcher": "starts_with"}),
        ] {
            assert!(
                serde_json::from_value::<BrowserStep>(missing.clone()).is_err(),
                "expected rejection: {missing}"
            );
        }
    }

    #[test]
    fn test_step_eval_serde() {
        let json_str = r#"{"action": "eval", "expression": "document.title"}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::Eval {
                expression,
                timeout_ms,
            } => {
                assert_eq!(expression, "document.title");
                assert_eq!(timeout_ms, None);
            }
            _ => panic!("Expected Eval"),
        }
    }

    #[test]
    fn test_step_press_key_serde() {
        let json_str = r#"{"action": "press_key", "key": "Enter", "modifiers": ["Ctrl"]}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::PressKey { key, modifiers } => {
                assert_eq!(key, "Enter");
                assert_eq!(modifiers, vec!["Ctrl"]);
            }
            _ => panic!("Expected PressKey"),
        }
    }

    #[test]
    fn test_step_screenshot_serde() {
        let json_str = r#"{"action": "screenshot"}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        assert!(matches!(step, BrowserStep::Screenshot { .. }));
    }

    #[test]
    fn accessibility_snapshot_options_default_to_ai_refs_and_round_trip() {
        let step: BrowserStep =
            serde_json::from_str(r#"{"action":"accessibility_snapshot"}"#).unwrap();
        match step {
            BrowserStep::AccessibilitySnapshot { options } => {
                assert_eq!(options.mode, AccessibilitySnapshotMode::Ai);
                assert!(options.refs_enabled());
                assert!(!options.boxes);
                assert!(options.locator.is_none());
                assert!(options.depth.is_none());
                assert!(options.max_chars.is_none());
            }
            _ => panic!("Expected AccessibilitySnapshot"),
        }

        let value = serde_json::json!({
            "action": "accessibility_snapshot",
            "mode": "default",
            "refs": true,
            "boxes": true,
            "locator": {"by": "css", "value": "main"},
            "depth": 3,
            "max_chars": 4096
        });
        let step: BrowserStep = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(step).unwrap(), value);
    }

    #[test]
    fn accessibility_snapshot_accepts_the_legacy_root_alias() {
        let step: BrowserStep = serde_json::from_value(serde_json::json!({
            "action": "accessibility_snapshot",
            "root": {"by": "css", "value": "main"}
        }))
        .unwrap();
        let BrowserStep::AccessibilitySnapshot { options } = step else {
            panic!("Expected AccessibilitySnapshot");
        };
        assert_eq!(
            options.locator,
            Some(BrowserLocator::css("main")),
            "legacy root spelling must still deserialize"
        );
        assert_eq!(
            serde_json::to_value(BrowserStep::AccessibilitySnapshot { options }).unwrap()
                ["locator"],
            serde_json::json!({"by": "css", "value": "main"}),
            "only the canonical locator name is emitted"
        );
    }

    #[test]
    fn accessibility_snapshot_scoping_options_compose() {
        let step: BrowserStep = serde_json::from_value(serde_json::json!({
            "action": "accessibility_snapshot",
            "locator": {"by": "css", "value": "#menu"},
            "depth": 2,
            "boxes": true
        }))
        .unwrap();
        let BrowserStep::AccessibilitySnapshot { options } = step else {
            panic!("Expected AccessibilitySnapshot");
        };
        assert_eq!(options.locator, Some(BrowserLocator::css("#menu")));
        assert_eq!(options.depth, Some(2));
        assert!(options.boxes);
        assert!(options.refs_enabled());
    }

    #[test]
    fn accessibility_snapshot_omitting_scoping_options_stays_byte_stable() {
        let options = AccessibilitySnapshotOptions::default();
        assert_eq!(
            serde_json::to_string(&BrowserStep::AccessibilitySnapshot { options }).unwrap(),
            r#"{"action":"accessibility_snapshot","mode":"ai","boxes":false}"#
        );
    }

    #[test]
    fn test_step_handle_dialog_serde() {
        let json_str = r#"{"action": "handle_dialog", "accept": true, "prompt_text": "answer"}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::HandleDialog {
                accept,
                prompt_text,
            } => {
                assert!(accept);
                assert_eq!(prompt_text.as_deref(), Some("answer"));
            }
            _ => panic!("Expected HandleDialog"),
        }
    }

    #[test]
    fn test_step_dismiss_overlays_serde() {
        let json_str = r#"{"action": "dismiss_overlays"}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        assert!(matches!(
            step,
            BrowserStep::DismissOverlays { aggressive: false }
        ));

        let opt_in: BrowserStep =
            serde_json::from_str(r#"{"action": "dismiss_overlays", "aggressive": true}"#).unwrap();
        assert!(matches!(
            opt_in,
            BrowserStep::DismissOverlays { aggressive: true }
        ));
    }

    #[test]
    fn test_locator_handler_steps_serde() {
        let json_str = r##"{
            "action": "add_locator_handler",
            "name": "cookie",
            "locator": {"by": "css", "value": "#accept"},
            "handler": {"type": "click"},
            "times": 2,
            "no_wait_after": true
        }"##;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::AddLocatorHandler {
                name,
                handler,
                times,
                no_wait_after,
                ..
            } => {
                assert_eq!(name, "cookie");
                assert!(matches!(handler, LocatorHandlerAction::Click));
                assert_eq!(times, Some(2));
                assert!(no_wait_after);
            }
            _ => panic!("expected add_locator_handler"),
        }

        let remove: BrowserStep =
            serde_json::from_str(r#"{"action":"remove_locator_handler","name":"cookie"}"#).unwrap();
        assert!(matches!(
            remove,
            BrowserStep::RemoveLocatorHandler { name } if name == "cookie"
        ));
    }

    #[test]
    fn test_step_wait_seconds_serde() {
        let json_str = r#"{"action": "wait_seconds", "seconds": 2.5}"#;
        let step: BrowserStep = serde_json::from_str(json_str).unwrap();
        match step {
            BrowserStep::WaitSeconds { seconds } => {
                assert!((seconds - 2.5).abs() < f64::EPSILON);
            }
            _ => panic!("Expected WaitSeconds"),
        }
    }

    #[test]
    fn test_full_request_serde() {
        let json_str = r#"{
            "session": "shared_default",
            "target": {"type": "active"},
            "steps": [
                {"action": "navigate", "url": "https://www.google.com"},
                {"action": "fill", "locator": {"by": "name", "value": "q"}, "text": "rust tokio tutorial"},
                {"action": "press_key", "key": "Enter"},
                {"action": "wait_for_url", "contains": "/search"},
                {"action": "extract_links", "limit": 10}
            ]
        }"#;
        let req: BrowserActionRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.session, SessionPolicy::SharedDefault);
        assert_eq!(req.target, TabTarget::Active);
        assert_eq!(req.steps.len(), 5);
    }

    #[test]
    fn test_request_defaults() {
        let json_str = r#"{"steps": [{"action": "screenshot"}]}"#;
        let req: BrowserActionRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.session, SessionPolicy::SharedDefault);
        assert_eq!(req.target, TabTarget::Active);
        assert!(req.attach_screenshot.is_none());
    }

    #[test]
    fn test_request_attach_screenshot_is_tri_state() {
        let omitted: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
        assert_eq!(omitted.attach_screenshot, None);

        let enabled: BrowserActionRequest =
            serde_json::from_str(r#"{"attach_screenshot": true, "steps": []}"#).unwrap();
        assert_eq!(enabled.attach_screenshot, Some(true));

        let suppressed: BrowserActionRequest =
            serde_json::from_str(r#"{"attach_screenshot": false, "steps": []}"#).unwrap();
        assert_eq!(suppressed.attach_screenshot, Some(false));
    }

    #[test]
    fn test_page_context_mode_defaults_to_snapshot_and_splits_the_two_attachments() {
        let omitted: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
        assert_eq!(omitted.page_context, None);
        assert_eq!(omitted.page_context_mode(), PageContextMode::Snapshot);
        assert_eq!(PageContextMode::default(), PageContextMode::Snapshot);

        for (mode, snapshot, screenshot) in [
            (PageContextMode::Snapshot, true, false),
            (PageContextMode::Screenshot, false, true),
            (PageContextMode::Both, true, true),
            (PageContextMode::None, false, false),
        ] {
            assert_eq!(mode.includes_snapshot(), snapshot, "{mode:?}");
            assert_eq!(mode.includes_screenshot(), screenshot, "{mode:?}");
        }
    }

    #[test]
    fn test_page_context_is_omitted_from_the_wire_when_unset() {
        let request: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["page_context"], serde_json::Value::Null);
    }

    #[test]
    fn test_execution_report_page_block_is_additive_for_existing_clients() {
        let legacy = serde_json::json!({
            "ok": true,
            "steps": [{"step_index": 0, "ok": true, "summary": "Navigated"}],
            "url": "https://example.com",
            "title": "Example",
            "dialogs": [],
            "new_tabs": [],
        });
        let report: ExecutionReport = serde_json::from_value(legacy).unwrap();
        assert!(report.page.is_none());
        assert!(report.steps[0].locator_echo.is_none());

        let serialized = serde_json::to_value(&report).unwrap();
        assert!(serialized.get("page").is_none());
        assert!(serialized["steps"][0].get("locator_echo").is_none());
        assert_eq!(serialized["url"], "https://example.com");
        assert_eq!(serialized["title"], "Example");
    }

    #[test]
    fn test_page_context_round_trips_the_header_and_the_snapshot_pointer() {
        let page = BrowserPageContext {
            status: Some(404),
            console: BrowserConsoleCounts {
                errors: 1,
                warnings: 0,
            },
            snapshot: Some(BrowserPageSnapshot {
                yaml: "- heading \"Not Found\" [ref=e1]".to_string(),
                lines: 1,
                bytes: 12_000,
                truncated: true,
                artifact: Some(BrowserSnapshotArtifact {
                    kind: "aria_snapshot".to_string(),
                    mime: "text/yaml".to_string(),
                    path: std::path::PathBuf::from("/tmp/artifacts/snapshot-1.yaml"),
                    bytes: 12_000,
                }),
                frames: Vec::new(),
            }),
        };

        let value = serde_json::to_value(&page).unwrap();
        assert!(value["snapshot"].get("frames").is_none());
        assert_eq!(value["status"], 404);
        assert_eq!(value["console"]["errors"], 1);
        assert_eq!(value["snapshot"]["truncated"], true);
        assert_eq!(value["snapshot"]["artifact"]["kind"], "aria_snapshot");
        assert_eq!(value["snapshot"]["artifact"]["mime"], "text/yaml");
        assert_eq!(
            serde_json::from_value::<BrowserPageContext>(value).unwrap(),
            page
        );
    }

    #[test]
    fn test_step_result_carries_the_canonical_locator_echo() {
        let step = StepResult::success(0, "click on <button>")
            .with_locator_echo(Some("getByRole('button', { name: 'Save' })".to_string()));

        let value = serde_json::to_value(&step).unwrap();
        assert_eq!(
            value["locator_echo"],
            "getByRole('button', { name: 'Save' })"
        );
        assert!(serde_json::to_value(StepResult::success(0, "noop"))
            .unwrap()
            .get("locator_echo")
            .is_none());
    }

    #[test]
    fn test_request_network_mode_defaults_to_summary() {
        let omitted: BrowserActionRequest = serde_json::from_str(r#"{"steps": []}"#).unwrap();
        assert_eq!(omitted.network, NetworkReportMode::Summary);
        assert_eq!(NetworkReportMode::default(), NetworkReportMode::Summary);
    }

    #[test]
    fn test_request_network_mode_accepts_every_lowercase_variant() {
        for (raw, expected) in [
            ("none", NetworkReportMode::None),
            ("summary", NetworkReportMode::Summary),
            ("full", NetworkReportMode::Full),
        ] {
            let parsed: BrowserActionRequest =
                serde_json::from_str(&format!(r#"{{"network": "{raw}", "steps": []}}"#)).unwrap();
            assert_eq!(parsed.network, expected);
            assert_eq!(serde_json::to_value(expected).unwrap(), raw);
        }
        assert!(serde_json::from_str::<BrowserActionRequest>(
            r#"{"network": "Full", "steps": []}"#
        )
        .is_err());
    }

    #[test]
    fn test_step_result_success() {
        let r = StepResult::success(0, "Navigated to https://example.com");
        assert!(r.ok);
        assert_eq!(r.step_index, 0);
        assert!(r.error.is_none());
    }

    #[test]
    fn test_step_result_failure() {
        let r = StepResult::failure(1, "Click failed", "Element not found: #btn");
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("Element not found: #btn"));
    }

    #[test]
    fn test_step_result_with_data() {
        let r =
            StepResult::success(0, "Extracted").with_data(serde_json::json!(["link1", "link2"]));
        assert!(r.data.is_some());
    }

    #[test]
    fn step_result_actionability_round_trips_all_none_states() {
        let mut result = StepResult::failure(2, "Click failed", "Timed out");
        result.actionability = Some(ActionabilityDiagnostics {
            call_log: vec![
                "waiting for css=#submit".to_string(),
                "element is not stable".to_string(),
            ],
            timed_out: true,
            elapsed_ms: Some(5_000),
            attempts: Some(3),
            attached: None,
            visible: None,
            stable: None,
            enabled: None,
            editable: None,
            receives_events: None,
            intercepting_element: None,
        });

        let json = serde_json::to_value(&result).unwrap();
        let actionability = json["actionability"].as_object().unwrap();
        assert_eq!(actionability["call_log"].as_array().unwrap().len(), 2);
        assert_eq!(actionability["timed_out"], true);
        assert_eq!(actionability["elapsed_ms"], 5_000);
        assert_eq!(actionability["attempts"], 3);
        for omitted in [
            "attached",
            "visible",
            "stable",
            "enabled",
            "editable",
            "receives_events",
            "intercepting_element",
        ] {
            assert!(!actionability.contains_key(omitted), "{omitted}");
        }

        let parsed: StepResult = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.actionability, result.actionability);
    }

    #[test]
    fn step_result_without_actionability_keeps_the_legacy_wire_shape() {
        let json = serde_json::to_value(StepResult::success(0, "Clicked")).unwrap();

        assert!(json.get("actionability").is_none());
    }

    #[test]
    fn test_field_kind_serde() {
        let kinds = vec![
            FieldKind::TextInput,
            FieldKind::PasswordInput,
            FieldKind::SearchInput,
            FieldKind::Textarea,
            FieldKind::Select,
            FieldKind::ContentEditable,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: FieldKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn test_fill_strategy_serde() {
        let strategies = vec![
            FillStrategy::NativeTyping,
            FillStrategy::DomValueSetter,
            FillStrategy::NativePrototypeSetter,
            FillStrategy::ContentEditablePath,
            FillStrategy::ClickAndType,
        ];
        for s in strategies {
            let json = serde_json::to_string(&s).unwrap();
            let parsed: FillStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, s);
        }
    }

    #[test]
    fn test_execution_report_serde() {
        let report = ExecutionReport {
            ok: true,
            warnings: vec!["browser session was dead; relaunched and retried".to_string()],
            steps: vec![
                StepResult::success(0, "nav ok"),
                StepResult::success(1, "click ok"),
            ],
            url: Some("https://example.com".to_string()),
            title: Some("Example".to_string()),
            page: Some(BrowserPageContext {
                status: Some(404),
                console: BrowserConsoleCounts {
                    errors: 1,
                    warnings: 2,
                },
                snapshot: None,
            }),
            stabilized: true,
            console: vec![],
            page_errors: vec![],
            network: vec![],
            network_summary: vec![],
            websockets: vec![],
            locator_handlers: vec![],
            dialogs: vec![DialogInfo {
                dialog_type: DialogType::Prompt,
                message: "Enter password=[REDACTED]".to_string(),
                default_value: "default".to_string(),
                action: DialogAction::Accepted,
                automatic: false,
            }],
            uploads: vec![UploadInfo {
                paths: vec!["/workspace/fixture.txt".to_string()],
                source: "direct".to_string(),
                in_memory_payloads: false,
            }],
            downloads: vec![DownloadInfo {
                guid: "download-guid".to_string(),
                url: "https://example.com/fixture.txt".to_string(),
                frame_id: "frame".to_string(),
                suggested_filename: "fixture.txt".to_string(),
                local_path: "/runtime/download-guid".to_string(),
                received_bytes: 7,
                total_bytes: 7,
                state: DownloadState::Completed,
                failure_reason: None,
            }],
            new_tabs: vec![],
            active_routes: vec![],
            intercepted_requests: vec![],
            context: None,
            screenshot: None,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["ok"].as_bool().unwrap());
        assert_eq!(json["steps"].as_array().unwrap().len(), 2);
        assert_eq!(json["dialogs"][0]["type"], "prompt");
        assert_eq!(json["dialogs"][0]["action"], "accepted");
        assert_eq!(json["dialogs"][0]["automatic"], false);
        assert_eq!(json["uploads"][0]["source"], "direct");
        assert_eq!(json["downloads"][0]["state"], "completed");
        assert_eq!(json["page"]["status"], 404);
        assert_eq!(json["page"]["console"]["errors"], 1);
        assert_eq!(json["page"]["console"]["warnings"], 2);
        assert!(json["page"].get("snapshot").is_none());
        let parsed: ExecutionReport = serde_json::from_value(json).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.page, report.page);
        assert_eq!(parsed.warnings, report.warnings);
    }

    #[test]
    fn execution_report_warnings_stay_optional_on_the_wire() {
        let without_warnings = ExecutionReport {
            warnings: Vec::new(),
            ..serde_json::from_value::<ExecutionReport>(serde_json::json!({
                "ok": true,
                "steps": [],
                "dialogs": [],
                "new_tabs": [],
            }))
            .unwrap()
        };

        let json = serde_json::to_value(&without_warnings).unwrap();
        assert!(json.get("warnings").is_none());

        let legacy: ExecutionReport = serde_json::from_value(serde_json::json!({
            "ok": true,
            "steps": [],
            "dialogs": [],
            "new_tabs": [],
        }))
        .unwrap();
        assert!(legacy.warnings.is_empty());
    }

    #[test]
    fn test_element_info_parse_from_js_json() {
        let json_str = r#"{
            "tag": "input",
            "input_type": "text",
            "id": "email",
            "name": "email",
            "placeholder": "Enter email",
            "aria_label": null,
            "role": null,
            "visible": true,
            "enabled": true,
            "readonly": false,
            "content_editable": false,
            "value": "",
            "inner_text": null,
            "bbox": {"x": 100.0, "y": 200.0, "width": 300.0, "height": 40.0},
            "field_kind": "text_input"
        }"#;
        let info: ElementInfo = serde_json::from_str(json_str).unwrap();
        assert_eq!(info.tag, "input");
        assert_eq!(info.input_type.as_deref(), Some("text"));
        assert!(info.visible);
        assert!(info.enabled);
        assert_eq!(info.field_kind, FieldKind::TextInput);
    }

    #[test]
    fn test_tab_info_serde() {
        let ti = TabInfo {
            id: "1".to_string(),
            target_id: "ABC123".to_string(),
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            active: true,
            opener: Some(TabOpener {
                tab_id: "opener".to_string(),
                frame_id: Some("frame".to_string()),
            }),
            opened_by_step: Some(2),
        };
        let json = serde_json::to_value(&ti).unwrap();
        assert_eq!(json["id"], "1");
        assert!(json["active"].as_bool().unwrap());
        assert_eq!(json["opener"]["tab_id"], "opener");
    }
}

const MAX_STEP_PARSE_ERROR_CHARS: usize = 400;
const MAX_ACTION_SUGGESTIONS: usize = 3;
const MIN_ACTION_SUGGESTION_SCORE: usize = 3;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserActionRequestEnvelope {
    #[serde(default)]
    session: SessionPolicy,
    #[serde(default)]
    target: TabTarget,
    #[serde(default)]
    attach_screenshot: Option<bool>,
    #[serde(default)]
    page_context: Option<PageContextMode>,
    #[serde(default)]
    network: NetworkReportMode,
    #[serde(default)]
    block_service_workers: Option<bool>,
    #[serde(default)]
    continue_on_error: bool,
    steps: Vec<Value>,
}

pub fn parse_browser_action_request(value: Value) -> Result<BrowserActionRequest, String> {
    let envelope: BrowserActionRequestEnvelope =
        serde_json::from_value(value).map_err(|error| bounded_error_text(&error.to_string()))?;
    let mut steps = Vec::with_capacity(envelope.steps.len());
    for (index, raw_step) in envelope.steps.into_iter().enumerate() {
        steps.push(parse_browser_step(index, raw_step)?);
    }
    Ok(BrowserActionRequest {
        session: envelope.session,
        target: envelope.target,
        attach_screenshot: envelope.attach_screenshot,
        page_context: envelope.page_context,
        network: envelope.network,
        block_service_workers: envelope.block_service_workers,
        continue_on_error: envelope.continue_on_error,
        steps,
    })
}

fn parse_browser_step(index: usize, raw_step: Value) -> Result<BrowserStep, String> {
    let action = raw_step
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(action), Some(object)) = (action.as_deref(), raw_step.as_object()) {
        if let Some(stray) = stray_field_on_fieldless_action(action, object) {
            return Err(format!(
                "step[{index}] ({action}): unknown field `{}`; `{action}` takes no fields",
                bounded_error_text(stray)
            ));
        }
    }
    serde_json::from_value::<BrowserStep>(raw_step).map_err(|error| match action {
        Some(action) if BrowserStep::ACTION_NAMES.contains(&action.as_str()) => format!(
            "step[{index}] ({action}): {}",
            bounded_error_text(&error.to_string())
        ),
        Some(action) => format!(
            "step[{index}]: unknown action '{}'{}",
            bounded_error_text(&action),
            format_action_suggestions(&action)
        ),
        None => format!("step[{index}]: {}", bounded_error_text(&error.to_string())),
    })
}

fn stray_field_on_fieldless_action<'a>(
    action: &str,
    object: &'a serde_json::Map<String, Value>,
) -> Option<&'a str> {
    let (_, required, optional, aliases) = BrowserStep::ACTION_FIELDS
        .iter()
        .find(|(name, ..)| *name == action)?;
    if !(required.is_empty() && optional.is_empty() && aliases.is_empty()) {
        return None;
    }
    object
        .keys()
        .find(|key| key.as_str() != "action")
        .map(String::as_str)
}

fn format_action_suggestions(action: &str) -> String {
    let lowered = action.to_ascii_lowercase().replace('-', "_");
    let max_distance = (lowered.len() / 3).max(2);
    let mut scored = BrowserStep::ACTION_NAMES
        .iter()
        .filter_map(|candidate| {
            let distance = edit_distance(&lowered, candidate);
            let prefix = shared_prefix_len(&lowered, candidate);
            let tier = if candidate.starts_with(&lowered) || lowered.starts_with(candidate) {
                0
            } else if distance <= max_distance {
                1
            } else if prefix >= MIN_ACTION_SUGGESTION_SCORE {
                2
            } else {
                return None;
            };
            Some((tier, distance, prefix, *candidate))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(right.2.cmp(&left.2))
            .then(left.3.cmp(right.3))
    });
    let names = scored
        .into_iter()
        .take(MAX_ACTION_SUGGESTIONS)
        .map(|(_, _, _, candidate)| candidate)
        .collect::<Vec<_>>();
    if names.is_empty() {
        String::new()
    } else {
        format!("; did you mean {}", names.join(", "))
    }
}

fn shared_prefix_len(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .take_while(|(left, right)| left == right)
        .count()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn bounded_error_text(message: &str) -> String {
    let collapsed = refact_core::string_utils::redact_sensitive(message)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= MAX_STEP_PARSE_ERROR_CHARS {
        return collapsed;
    }
    let kept = collapsed
        .chars()
        .take(MAX_STEP_PARSE_ERROR_CHARS)
        .collect::<String>();
    format!("{kept}...")
}

#[cfg(test)]
mod browser_action_parse_tests {
    use super::*;


    fn parse_step(step: Value) -> BrowserStep {
        parse_browser_action_request(serde_json::json!({"steps": [step]}))
            .unwrap()
            .steps
            .remove(0)
    }

    fn round_trip(step: Value) -> Value {
        let serialized = serde_json::to_value(parse_step(step)).unwrap();
        let reparsed = serde_json::to_value(parse_step(serialized.clone())).unwrap();
        assert_eq!(serialized, reparsed, "round-trip drifted");
        serialized
    }

    fn assert_step_rejection(step: Value, expected: &str) {
        let error =
            parse_browser_action_request(serde_json::json!({"steps": [step.clone()]})).unwrap_err();
        assert!(
            error.starts_with("step[0] ("),
            "expected an action-qualified error for {step}: {error}"
        );
        assert!(
            error.contains(expected),
            "unexpected error for {step}: {error}"
        );
    }

    fn valid_request() -> Value {
        serde_json::json!({
            "attach_screenshot": true,
            "network": "full",
            "steps": [
                {"action": "accessibility_snapshot"},
                {"action": "click", "locator": {"by": "ref", "value": "e5"}},
                {"action": "send_web_socket_message", "pattern": "wss://example.com/**", "text": "hi"}
            ]
        })
    }

    #[test]
    fn fieldless_actions_reject_stray_fields_like_every_other_action() {
        assert_step_rejection(
            serde_json::json!({"action": "reload", "bogus_field_xyz": 1}),
            "unknown field `bogus_field_xyz`; `reload` takes no fields",
        );
        assert_step_rejection(
            serde_json::json!({"action": "list_tabs", "timeout_ms": 5000}),
            "`list_tabs` takes no fields",
        );
        assert_step_rejection(
            serde_json::json!({"action": "tab_log", "locator": {"by": "css", "value": "#x"}}),
            "`tab_log` takes no fields",
        );
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "reload"}, {"action": "hide_highlight"}],
            "continue_on_error": true
        }))
        .unwrap();
        assert_eq!(request.steps.len(), 2);
        assert!(request.continue_on_error);
    }

    #[test]
    fn missing_required_field_names_the_step_index_and_action() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "accessibility_snapshot"},
                {"action": "click", "locator": {"by": "ref", "value": "e5"}},
                {"action": "send_web_socket_message", "text": "hi"}
            ]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[2] (send_web_socket_message): "),
            "unexpected error: {error}"
        );
        assert!(error.contains("pattern"), "unexpected error: {error}");
        assert!(!error.contains('\n'), "unexpected error: {error}");
    }

    #[test]
    fn unknown_action_names_the_step_index_and_suggests_valid_actions() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "navigate", "url": "https://example.com"},
                {"action": "clic", "locator": {"by": "ref", "value": "e5"}}
            ]
        }))
        .unwrap_err();

        assert_eq!(
            error,
            "step[1]: unknown action 'clic'; did you mean click, click_if_exists"
        );

        let typo_in_the_first_letters = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "sceenshot"}]
        }))
        .unwrap_err();
        assert_eq!(
            typo_in_the_first_letters,
            "step[0]: unknown action 'sceenshot'; did you mean screenshot"
        );

        let dashed = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "wait-for-selector", "locator": {"by": "css", "value": "#x"}}]
        }))
        .unwrap_err();
        assert!(
            dashed.contains("did you mean wait_for_selector"),
            "{dashed}"
        );
    }

    #[test]
    fn step_without_an_action_is_still_indexed() {
        let error =
            parse_browser_action_request(serde_json::json!({"steps": [{"url": "https://x.dev"}]}))
                .unwrap_err();

        assert!(error.starts_with("step[0]: "), "unexpected error: {error}");
        assert!(error.contains("action"), "unexpected error: {error}");
    }

    #[test]
    fn valid_batches_parse_identically_to_the_single_shot_parse() {
        let request = valid_request();
        let two_phase = parse_browser_action_request(request.clone()).unwrap();
        let single_shot: BrowserActionRequest = serde_json::from_value(request).unwrap();

        assert_eq!(
            serde_json::to_value(&two_phase).unwrap(),
            serde_json::to_value(&single_shot).unwrap()
        );
    }

    #[test]
    fn envelope_defaults_and_rejections_match_the_single_shot_parse() {
        let minimal = serde_json::json!({"steps": []});
        assert_eq!(
            serde_json::to_value(parse_browser_action_request(minimal.clone()).unwrap()).unwrap(),
            serde_json::to_value(
                serde_json::from_value::<BrowserActionRequest>(minimal.clone()).unwrap()
            )
            .unwrap()
        );

        for rejected in [
            serde_json::json!({}),
            serde_json::json!({"steps": {}}),
            serde_json::json!({"steps": [], "network": "Full"}),
            serde_json::json!({"steps": [], "target": {"type": "id"}}),
        ] {
            assert!(
                parse_browser_action_request(rejected.clone()).is_err(),
                "expected rejection: {rejected}"
            );
            assert!(
                serde_json::from_value::<BrowserActionRequest>(rejected.clone()).is_err(),
                "expected rejection: {rejected}"
            );
        }
    }

    #[test]
    fn renamed_fields_accept_both_canonical_and_legacy_names() {
        for (canonical, legacy) in [
            (
                serde_json::json!({"action": "send_web_socket_message", "pattern": "wss://example.com/**", "text": "hi"}),
                serde_json::json!({"action": "send_web_socket_message", "url_pattern": "wss://example.com/**", "data": "hi"}),
            ),
            (
                serde_json::json!({"action": "wait_for_url", "pattern": "/done"}),
                serde_json::json!({"action": "wait_for_url", "contains": "/done"}),
            ),
            (
                serde_json::json!({"action": "wait_for_request", "pattern": "/api"}),
                serde_json::json!({"action": "wait_for_request", "url_or_pattern": "/api"}),
            ),
            (
                serde_json::json!({"action": "wait_for_response", "pattern": {"source": "/api", "flags": "i"}}),
                serde_json::json!({"action": "wait_for_response", "url_or_pattern": {"source": "/api", "flags": "i"}}),
            ),
            (
                serde_json::json!({"action": "accessibility_snapshot", "locator": {"by": "css", "value": "main"}}),
                serde_json::json!({"action": "accessibility_snapshot", "root": {"by": "css", "value": "main"}}),
            ),
        ] {
            let from_canonical =
                parse_browser_action_request(serde_json::json!({"steps": [canonical.clone()]}))
                    .unwrap();
            let from_legacy =
                parse_browser_action_request(serde_json::json!({"steps": [legacy.clone()]}))
                    .unwrap();
            let canonical_value = serde_json::to_value(&from_canonical).unwrap();
            assert_eq!(
                canonical_value,
                serde_json::to_value(&from_legacy).unwrap(),
                "alias mismatch for {legacy}"
            );

            let serialized = serde_json::to_string(&canonical_value).unwrap();
            for legacy_name in [
                "url_pattern",
                "url_or_pattern",
                "contains",
                "\"data\"",
                "\"root\"",
            ] {
                assert!(
                    !serialized.contains(legacy_name),
                    "serialization must emit canonical names only, got {serialized}"
                );
            }
        }
    }

    #[test]
    fn canonical_and_legacy_names_together_are_rejected() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "wait_for_request", "pattern": "/api", "url_or_pattern": "/api"}]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[0] (wait_for_request): "),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("duplicate field"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accessibility_snapshot_scoping_options_compose_in_a_batch() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [{
                "action": "accessibility_snapshot",
                "locator": {"by": "css", "value": "#dropdown"},
                "depth": 2,
                "boxes": true
            }]
        }))
        .unwrap();

        let BrowserStep::AccessibilitySnapshot { options } = &request.steps[0] else {
            panic!("Expected AccessibilitySnapshot");
        };
        assert_eq!(options.locator, Some(BrowserLocator::css("#dropdown")));
        assert_eq!(options.depth, Some(2));
        assert!(options.boxes);
    }

    #[test]
    fn accessibility_snapshot_rejects_root_and_locator_together() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [{
                "action": "accessibility_snapshot",
                "locator": {"by": "css", "value": "main"},
                "root": {"by": "css", "value": "main"}
            }]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[0] (accessibility_snapshot): "),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("duplicate field"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_step_fields_are_rejected_with_the_step_index_and_action() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "navigate", "url": "https://example.com"},
                {"action": "extract_table", "locator": {"by": "css", "value": "table"}, "limitt": 5}
            ]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[1] (extract_table): "),
            "unexpected error: {error}"
        );
        assert!(error.contains("limitt"), "unexpected error: {error}");
    }

    #[test]
    fn unknown_fields_are_rejected_on_flattened_option_steps() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "screenshot", "full_page": true, "save_ass": "/tmp/x.png"}]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[0] (screenshot): "),
            "unexpected error: {error}"
        );
        assert!(error.contains("save_ass"), "unexpected error: {error}");

        parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "screenshot", "full_page": true, "type": "png"}]
        }))
        .unwrap();
    }

    #[test]
    fn reset_takes_no_parameters_and_batches_with_other_steps() {
        let request =
            parse_browser_action_request(serde_json::json!({"steps": [{"action": "reset"}]}))
                .unwrap();

        assert!(matches!(request.steps.as_slice(), [BrowserStep::Reset]));
        assert_eq!(
            serde_json::to_value(&request.steps[0]).unwrap(),
            serde_json::json!({"action": "reset"})
        );

        let batched = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "route", "pattern": "**/api/**", "handler": {"type": "abort", "reason": "failed"}},
                {"action": "reset"}
            ]
        }))
        .unwrap();

        assert!(matches!(
            batched.steps.as_slice(),
            [BrowserStep::Route { .. }, BrowserStep::Reset]
        ));
    }

    #[test]
    fn cdp_send_defaults_to_the_page_target_and_omits_absent_params() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "cdp_send", "method": "Browser.getVersion", "target": "browser"},
                {"action": "cdp_send", "method": "Runtime.evaluate", "params": {"expression": "1+1"}}
            ]
        }))
        .unwrap();

        let BrowserStep::CdpSend {
            method,
            params,
            target,
        } = &request.steps[0]
        else {
            panic!("expected a cdp_send step");
        };
        assert_eq!(method, "Browser.getVersion");
        assert_eq!(*params, None);
        assert_eq!(*target, CdpTarget::Browser);

        assert!(matches!(
            &request.steps[1],
            BrowserStep::CdpSend {
                target: CdpTarget::Page,
                params: Some(_),
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(&request.steps[1]).unwrap(),
            serde_json::json!({
                "action": "cdp_send",
                "method": "Runtime.evaluate",
                "params": {"expression": "1+1"},
                "target": "page"
            })
        );
    }

    #[test]
    fn cdp_send_requires_a_method_and_rejects_unknown_fields_and_targets() {
        for (step, expected) in [
            (
                serde_json::json!({"action": "cdp_send", "params": {}}),
                "method",
            ),
            (
                serde_json::json!({"action": "cdp_send", "method": "Page.reload", "sessionId": "x"}),
                "sessionId",
            ),
            (
                serde_json::json!({"action": "cdp_send", "method": "Page.reload", "target": "tab"}),
                "tab",
            ),
        ] {
            let error = parse_browser_action_request(serde_json::json!({"steps": [step.clone()]}))
                .unwrap_err();
            assert!(
                error.starts_with("step[0] (cdp_send): "),
                "unexpected error for {step}: {error}"
            );
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn tap_parses_both_the_locator_and_the_coordinate_shape() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "tap", "locator": {"by": "ref", "value": "e5"}},
                {"action": "tap", "x": 10.0, "y": 20.0}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::Tap {
                    locator: Some(_),
                    x: None,
                    y: None
                },
                BrowserStep::Tap {
                    locator: None,
                    x: Some(_),
                    y: Some(_)
                }
            ]
        ));
        assert_eq!(
            serde_json::to_value(&request.steps[1]).unwrap(),
            serde_json::json!({"action": "tap", "x": 10.0, "y": 20.0})
        );
    }

    #[test]
    fn insert_text_locator_is_optional_and_press_sequentially_requires_one() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "insert_text", "text": "hi"},
                {"action": "press_sequentially", "locator": {"by": "css", "value": "#q"}, "text": "hi"}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::InsertText { locator: None, .. },
                BrowserStep::PressSequentially { delay_ms: None, .. }
            ]
        ));

        let error = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "press_sequentially", "text": "hi"}]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[0] (press_sequentially): "),
            "unexpected error: {error}"
        );
        assert!(error.contains("locator"), "unexpected error: {error}");
    }

    #[test]
    fn http_request_defaults_to_a_bare_get_and_keeps_its_optional_fields() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "http_request", "url": "https://example.com/api/session"}]
        }))
        .unwrap();

        let BrowserStep::HttpRequest { options } = &request.steps[0] else {
            panic!("expected an http_request step");
        };
        assert_eq!(options.url, "https://example.com/api/session");
        assert_eq!(options.method, None);
        assert!(options.headers.is_empty());
        assert_eq!(options.body, None);
        assert_eq!(options.form, None);
        assert_eq!(options.fail_on_status, None);
        assert_eq!(
            serde_json::to_value(&request.steps[0]).unwrap(),
            serde_json::json!({"action": "http_request", "url": "https://example.com/api/session"})
        );

        let full = parse_browser_action_request(serde_json::json!({
            "steps": [{
                "action": "http_request",
                "url": "https://example.com/api/items",
                "method": "post",
                "headers": {"accept": "application/json"},
                "body_json": {"name": "widget"},
                "timeout_ms": 1_000,
                "max_redirects": 0,
                "fail_on_status": true,
                "full_headers": true
            }]
        }))
        .unwrap();
        let BrowserStep::HttpRequest { options } = &full.steps[0] else {
            panic!("expected an http_request step");
        };
        assert_eq!(options.method.as_deref(), Some("post"));
        assert_eq!(
            options.body_json,
            Some(serde_json::json!({"name": "widget"}))
        );
        assert_eq!(options.max_redirects, Some(0));
        assert_eq!(options.fail_on_status, Some(true));
        assert_eq!(options.full_headers, Some(true));
    }

    #[test]
    fn http_request_requires_a_url_and_rejects_unknown_fields() {
        let missing = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "http_request", "method": "GET"}]
        }))
        .unwrap_err();
        assert!(
            missing.starts_with("step[0] (http_request): "),
            "unexpected error: {missing}"
        );
        assert!(missing.contains("url"), "unexpected error: {missing}");

        let unknown = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "http_request", "url": "https://x.test/", "bodyy": "oops"}]
        }))
        .unwrap_err();
        assert!(
            unknown.starts_with("step[0] (http_request): "),
            "unexpected error: {unknown}"
        );
        assert!(unknown.contains("bodyy"), "unexpected error: {unknown}");
    }

    #[test]
    fn route_accepts_times_and_the_new_chain_handlers() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "route", "pattern": "**/api/**", "handler": {"type": "fulfill", "json": {"ok": true}}, "times": 2},
                {"action": "route", "pattern": "**/api/**", "handler": {"type": "fallback"}},
                {"action": "route", "pattern": "**/api/**", "handler": {"type": "fetch_and_fulfill", "status": 503}}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps[0],
            BrowserStep::Route {
                times: Some(2),
                handler: RouteHandler::Fulfill { status: 200, .. },
                ..
            }
        ));
        assert!(matches!(
            request.steps[1],
            BrowserStep::Route {
                times: None,
                handler: RouteHandler::Fallback,
                ..
            }
        ));
        assert!(matches!(
            request.steps[2],
            BrowserStep::Route {
                handler: RouteHandler::FetchAndFulfill {
                    status: Some(503),
                    ..
                },
                ..
            }
        ));

        assert_eq!(
            serde_json::to_value(&request.steps[1]).unwrap(),
            serde_json::json!({
                "action": "route",
                "pattern": "**/api/**",
                "handler": {"type": "fallback"}
            })
        );
    }

    #[test]
    fn fulfill_path_shorthand_round_trips_without_a_status() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "route", "pattern": "**/app.css", "handler": {"type": "fulfill", "path": "mock.css"}}]
        }))
        .unwrap();

        let BrowserStep::Route {
            handler: RouteHandler::Fulfill { status, path, .. },
            ..
        } = &request.steps[0]
        else {
            panic!("expected a fulfill route");
        };
        assert_eq!(*status, 200);
        assert_eq!(path.as_deref(), Some("mock.css"));
    }

    #[test]
    fn screencast_steps_parse_with_defaults_and_reject_unknown_fields() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "capture_frames"},
                {"action": "capture_frames", "duration_ms": 2000, "interval_ms": 500, "full_page": true},
                {"action": "screencast_start"},
                {"action": "screencast_stop", "compose": false}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::CaptureFrames {
                    duration_ms: None,
                    frame_count: None,
                    interval_ms: None,
                    locator: None,
                    full_page: None,
                },
                BrowserStep::CaptureFrames {
                    duration_ms: Some(2000),
                    interval_ms: Some(500),
                    full_page: Some(true),
                    ..
                },
                BrowserStep::ScreencastStart {
                    quality: None,
                    max_width: None,
                    max_height: None,
                },
                BrowserStep::ScreencastStop {
                    compose: Some(false)
                },
            ]
        ));
        assert_eq!(
            serde_json::to_value(&request.steps[0]).unwrap(),
            serde_json::json!({"action": "capture_frames"})
        );

        let error = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "capture_frames", "frames": 4}]
        }))
        .unwrap_err();

        assert!(
            error.starts_with("step[0] (capture_frames): unknown field `frames`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn page_context_defaults_to_snapshot_and_accepts_every_mode() {
        let omitted = parse_browser_action_request(serde_json::json!({"steps": []})).unwrap();
        assert_eq!(omitted.page_context, None);
        assert_eq!(omitted.page_context_mode(), PageContextMode::Snapshot);

        for (raw, expected) in [
            ("snapshot", PageContextMode::Snapshot),
            ("screenshot", PageContextMode::Screenshot),
            ("both", PageContextMode::Both),
            ("none", PageContextMode::None),
        ] {
            let parsed =
                parse_browser_action_request(serde_json::json!({"page_context": raw, "steps": []}))
                    .unwrap();
            assert_eq!(parsed.page_context, Some(expected));
            assert_eq!(parsed.page_context_mode(), expected);
            assert_eq!(serde_json::to_value(expected).unwrap(), raw);
        }
    }

    #[test]
    fn page_context_is_rejected_when_it_is_not_a_known_mode() {
        for rejected in [
            serde_json::json!({"page_context": "Snapshot", "steps": []}),
            serde_json::json!({"page_context": "aria", "steps": []}),
            serde_json::json!({"page_context": true, "steps": []}),
        ] {
            assert!(
                parse_browser_action_request(rejected.clone()).is_err(),
                "expected rejection: {rejected}"
            );
        }
    }

    #[test]
    fn page_context_composes_with_the_attach_screenshot_override() {
        let request = parse_browser_action_request(serde_json::json!({
            "page_context": "none",
            "attach_screenshot": true,
            "steps": [{"action": "navigate", "url": "https://example.com"}]
        }))
        .unwrap();

        assert_eq!(request.page_context_mode(), PageContextMode::None);
        assert_eq!(request.attach_screenshot, Some(true));
        assert_eq!(
            serde_json::to_value(&request).unwrap()["page_context"],
            serde_json::json!("none")
        );
    }

    #[test]
    fn unknown_batch_envelope_fields_are_rejected() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [],
            "netwrok": "full"
        }))
        .unwrap_err();

        assert!(error.contains("netwrok"), "unexpected error: {error}");
    }

    #[test]
    fn step_errors_are_single_line_and_bounded() {
        let error = parse_browser_action_request(serde_json::json!({
            "steps": [{"action": "x".repeat(4000)}]
        }))
        .unwrap_err();

        assert!(!error.contains('\n'), "unexpected error: {error}");
        assert!(error.chars().count() < 600, "unexpected error: {error}");
    }

    #[test]
    fn gallery_and_state_steps_parse_with_flattened_screenshot_options() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "screenshot_elements", "locators": [{"by": "css", "value": ".card"}], "compose": "separate", "mask": [{"by": "css", "value": ".secret"}]},
                {"action": "capture_element_states", "locator": {"by": "css", "value": "button"}, "states": ["hover", "active"], "labels": true}
            ]
        }))
        .unwrap();

        match &request.steps[0] {
            BrowserStep::ScreenshotElements {
                locators,
                compose,
                options,
                ..
            } => {
                assert_eq!(locators.len(), 1);
                assert_eq!(*compose, BrowserComposeMode::Separate);
                assert_eq!(options.mask.len(), 1);
            }
            other => panic!("unexpected step: {other:?}"),
        }
        match &request.steps[1] {
            BrowserStep::CaptureElementStates { states, labels, .. } => {
                assert_eq!(
                    states,
                    &[BrowserElementState::Hover, BrowserElementState::Active]
                );
                assert_eq!(*labels, Some(true));
            }
            other => panic!("unexpected step: {other:?}"),
        }
    }

    #[test]
    fn throttling_and_device_steps_parse_their_parameters() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "set_network_conditions", "preset": "slow-3g", "latency_ms": 120.0, "offline": false},
                {"action": "set_cpu_throttling", "rate": 4.0},
                {"action": "emulate_device", "name": "iPhone 13"},
                {"action": "list_devices"},
            ]
        }))
        .unwrap();

        match &request.steps[0] {
            BrowserStep::SetNetworkConditions {
                offline,
                latency_ms,
                download_kbps,
                upload_kbps,
                preset,
            } => {
                assert_eq!(*offline, Some(false));
                assert_eq!(*latency_ms, Some(120.0));
                assert_eq!(*download_kbps, None);
                assert_eq!(*upload_kbps, None);
                assert_eq!(preset.as_deref(), Some("slow-3g"));
            }
            other => panic!("unexpected step: {other:?}"),
        }
        assert!(matches!(
            request.steps[1],
            BrowserStep::SetCpuThrottling { rate } if rate == 4.0
        ));
        assert!(matches!(
            &request.steps[2],
            BrowserStep::EmulateDevice { name } if name == "iPhone 13"
        ));
        assert!(matches!(
            &request.steps[3],
            BrowserStep::ListDevices { filter: None }
        ));
    }

    #[test]
    fn throttling_and_device_steps_reject_unknown_and_missing_fields() {
        for (step, expected) in [
            (
                serde_json::json!({"action": "set_network_conditions", "latencyMs": 100}),
                "latencyMs",
            ),
            (serde_json::json!({"action": "set_cpu_throttling"}), "rate"),
            (serde_json::json!({"action": "emulate_device"}), "name"),
            (
                serde_json::json!({"action": "list_devices", "contains": "pixel"}),
                "contains",
            ),
        ] {
            let error = parse_browser_action_request(serde_json::json!({"steps": [step.clone()]}))
                .unwrap_err();
            assert!(error.starts_with("step[0] ("), "unexpected error: {error}");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn gallery_and_state_steps_reject_unknown_fields_and_bad_enum_values() {
        for (step, expected) in [
            (
                serde_json::json!({"action": "screenshot_elements", "locators": [], "composee": "grid"}),
                "composee",
            ),
            (
                serde_json::json!({"action": "screenshot_elements", "locators": [], "compose": "mosaic"}),
                "mosaic",
            ),
            (
                serde_json::json!({"action": "capture_element_states", "locator": {"by": "css", "value": "button"}, "states": ["visited"]}),
                "visited",
            ),
        ] {
            let error = parse_browser_action_request(serde_json::json!({"steps": [step.clone()]}))
                .unwrap_err();
            assert!(error.starts_with("step[0] ("), "unexpected error: {error}");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn page_manipulation_steps_parse_with_their_optional_fields() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "set_content", "html": "<p>hi</p>"},
                {"action": "set_content", "html": "<p>hi</p>", "wait_until": "networkidle"},
                {"action": "page_content"},
                {"action": "add_script_tag", "url": "https://example.com/a.js"},
                {"action": "add_script_tag", "content": "window.x = 1", "script_type": "module"},
                {"action": "add_style_tag", "url": "https://example.com/a.css"},
                {"action": "add_style_tag", "content": "body { color: red }"},
                {"action": "add_init_script", "content": "window.seeded = true"},
                {"action": "remove_init_script", "id": "init-1"},
                {"action": "dispatch_event", "locator": {"by": "css", "value": "#go"}, "event_type": "click"},
                {"action": "dispatch_event", "locator": {"by": "css", "value": "#go"}, "event_type": "click", "event_init": {"bubbles": true}}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::SetContent {
                    wait_until: None,
                    ..
                },
                BrowserStep::SetContent {
                    wait_until: Some(BrowserLoadState::Networkidle),
                    ..
                },
                BrowserStep::PageContent,
                BrowserStep::AddScriptTag {
                    url: Some(_),
                    content: None,
                    script_type: None,
                },
                BrowserStep::AddScriptTag {
                    url: None,
                    content: Some(_),
                    script_type: Some(_),
                },
                BrowserStep::AddStyleTag {
                    url: Some(_),
                    content: None,
                },
                BrowserStep::AddStyleTag {
                    url: None,
                    content: Some(_),
                },
                BrowserStep::AddInitScript { .. },
                BrowserStep::RemoveInitScript { .. },
                BrowserStep::DispatchEvent {
                    event_init: None,
                    ..
                },
                BrowserStep::DispatchEvent {
                    event_init: Some(_),
                    ..
                },
            ]
        ));

        assert_eq!(
            round_trip(serde_json::json!({"action": "set_content", "html": "<p>hi</p>"})),
            serde_json::json!({"action": "set_content", "html": "<p>hi</p>"})
        );
        assert_eq!(
            round_trip(serde_json::json!({"action": "page_content"})),
            serde_json::json!({"action": "page_content"})
        );
        assert_eq!(
            round_trip(
                serde_json::json!({"action": "add_script_tag", "url": "https://example.com/a.js"})
            ),
            serde_json::json!({"action": "add_script_tag", "url": "https://example.com/a.js"})
        );
        assert_eq!(
            round_trip(serde_json::json!({"action": "add_style_tag"})),
            serde_json::json!({"action": "add_style_tag"})
        );
        assert_eq!(
            round_trip(serde_json::json!({
                "action": "dispatch_event",
                "locator": {"by": "css", "value": "#go"},
                "event_type": "click"
            })),
            serde_json::json!({
                "action": "dispatch_event",
                "locator": {"by": "css", "value": "#go"},
                "event_type": "click"
            })
        );
    }

    #[test]
    fn page_manipulation_steps_reject_unknown_and_missing_fields() {
        for (step, expected) in [
            (serde_json::json!({"action": "set_content"}), "html"),
            (
                serde_json::json!({"action": "set_content", "html": "<p/>", "waitUntil": "load"}),
                "waitUntil",
            ),
            (
                serde_json::json!({"action": "set_content", "html": "<p/>", "wait_until": "idle"}),
                "idle",
            ),
            (
                serde_json::json!({"action": "add_script_tag", "type": "module"}),
                "type",
            ),
            (
                serde_json::json!({"action": "add_style_tag", "css": "body {}"}),
                "css",
            ),
            (serde_json::json!({"action": "add_init_script"}), "content"),
            (serde_json::json!({"action": "remove_init_script"}), "id"),
            (
                serde_json::json!({"action": "dispatch_event", "locator": {"by": "css", "value": "#go"}}),
                "event_type",
            ),
            (
                serde_json::json!({"action": "dispatch_event", "locator": {"by": "css", "value": "#go"}, "event_type": "click", "init": {}}),
                "init",
            ),
        ] {
            assert_step_rejection(step, expected);
        }
    }

    #[test]
    fn wait_step_filters_are_optional_and_round_trip() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "wait_for_request", "pattern": "/api"},
                {"action": "wait_for_request", "pattern": "/api", "method": "POST"},
                {"action": "wait_for_response", "pattern": "/api"},
                {"action": "wait_for_response", "pattern": "/api", "method": "GET", "status": 204},
                {"action": "wait_for_selector", "locator": {"by": "css", "value": "#q"}},
                {"action": "wait_for_selector", "locator": {"by": "css", "value": "#q"}, "state": "hidden"},
                {"action": "wait_for_console_message"},
                {"action": "wait_for_console_message", "contains": "ready", "level": "warning", "timeout_ms": 500}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::WaitForRequest { method: None, .. },
                BrowserStep::WaitForRequest {
                    method: Some(_),
                    ..
                },
                BrowserStep::WaitForResponse {
                    method: None,
                    status: None,
                    ..
                },
                BrowserStep::WaitForResponse {
                    method: Some(_),
                    status: Some(204),
                    ..
                },
                BrowserStep::WaitForSelector { state: None, .. },
                BrowserStep::WaitForSelector {
                    state: Some(BrowserWaitState::Hidden),
                    ..
                },
                BrowserStep::WaitForConsoleMessage {
                    contains: None,
                    level: None,
                    timeout_ms: None,
                },
                BrowserStep::WaitForConsoleMessage {
                    contains: Some(_),
                    level: Some(BrowserConsoleLevel::Warning),
                    timeout_ms: Some(500),
                },
            ]
        ));

        for state in ["attached", "detached", "visible", "hidden"] {
            let serialized = round_trip(serde_json::json!({
                "action": "wait_for_selector",
                "locator": {"by": "css", "value": "#q"},
                "state": state
            }));
            assert_eq!(serialized["state"], serde_json::json!(state));
        }
        for step in [
            serde_json::json!({"action": "wait_for_request", "pattern": "/api", "method": "POST"}),
            serde_json::json!({"action": "wait_for_response", "pattern": "/api", "status": 500}),
            serde_json::json!({"action": "wait_for_console_message", "level": "error"}),
        ] {
            round_trip(step);
        }
    }

    #[test]
    fn wait_step_filters_reject_unknown_fields_and_wrong_state_families() {
        for (step, expected) in [
            (
                serde_json::json!({"action": "wait_for_selector", "locator": {"by": "css", "value": "#q"}, "state": "networkidle"}),
                "networkidle",
            ),
            (
                serde_json::json!({"action": "wait_for_selector", "locator": {"by": "css", "value": "#q"}, "state": "load"}),
                "load",
            ),
            (
                serde_json::json!({"action": "wait_for_selector", "locator": {"by": "css", "value": "#q"}, "states": "visible"}),
                "states",
            ),
            (
                serde_json::json!({"action": "wait_for_request", "pattern": "/api", "status": 200}),
                "status",
            ),
            (
                serde_json::json!({"action": "wait_for_response", "pattern": "/api", "statusCode": 200}),
                "statusCode",
            ),
            (
                serde_json::json!({"action": "wait_for_console_message", "level": "debug"}),
                "debug",
            ),
            (
                serde_json::json!({"action": "wait_for_console_message", "text": "ready"}),
                "text",
            ),
        ] {
            assert_step_rejection(step, expected);
        }
    }

    #[test]
    fn web_socket_steps_default_to_mocked_forwarding() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "route_web_socket", "pattern": "wss://example.com/**"},
                {"action": "route_web_socket", "pattern": "wss://example.com/**", "mode": "intercept", "on_page_message": "drop", "on_server_message": "capture"},
                {"action": "close_web_socket", "pattern": "wss://example.com/**"},
                {"action": "close_web_socket", "pattern": "wss://example.com/**", "code": 1000, "reason": "bye"}
            ]
        }))
        .unwrap();

        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::RouteWebSocket {
                    mode: WebSocketRouteMode::Mock,
                    on_page_message: WebSocketMessageAction::Forward,
                    on_server_message: WebSocketMessageAction::Forward,
                    ..
                },
                BrowserStep::RouteWebSocket {
                    mode: WebSocketRouteMode::Intercept,
                    on_page_message: WebSocketMessageAction::Drop,
                    on_server_message: WebSocketMessageAction::Capture,
                    ..
                },
                BrowserStep::CloseWebSocket {
                    code: None,
                    reason: None,
                    ..
                },
                BrowserStep::CloseWebSocket {
                    code: Some(1000),
                    reason: Some(_),
                    ..
                },
            ]
        ));

        assert_eq!(
            round_trip(
                serde_json::json!({"action": "route_web_socket", "pattern": "wss://example.com/**"})
            ),
            serde_json::json!({
                "action": "route_web_socket",
                "pattern": "wss://example.com/**",
                "mode": "mock",
                "on_page_message": "forward",
                "on_server_message": "forward"
            })
        );
        assert_eq!(
            round_trip(
                serde_json::json!({"action": "close_web_socket", "pattern": "wss://example.com/**"})
            ),
            serde_json::json!({"action": "close_web_socket", "pattern": "wss://example.com/**"})
        );
    }

    #[test]
    fn web_socket_steps_reject_unknown_fields_and_bad_modes() {
        for (step, expected) in [
            (serde_json::json!({"action": "route_web_socket"}), "pattern"),
            (
                serde_json::json!({"action": "route_web_socket", "pattern": "wss://x/**", "mode": "record"}),
                "record",
            ),
            (
                serde_json::json!({"action": "route_web_socket", "pattern": "wss://x/**", "on_page_message": "ignore"}),
                "ignore",
            ),
            (
                serde_json::json!({"action": "route_web_socket", "pattern": "wss://x/**", "on_message": "drop"}),
                "on_message",
            ),
            (serde_json::json!({"action": "close_web_socket"}), "pattern"),
            (
                serde_json::json!({"action": "close_web_socket", "pattern": "wss://x/**", "close_code": 1000}),
                "close_code",
            ),
        ] {
            assert_step_rejection(step, expected);
        }
    }

    #[test]
    fn expect_polish_options_are_optional_and_round_trip() {
        let request = parse_browser_action_request(serde_json::json!({
            "steps": [
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_be_visible"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_be_visible"}, "not": true},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_be_checked"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_be_checked", "indeterminate": true}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_have_css", "name": "color", "expected": "rgb(0, 0, 0)"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_have_css", "name": "content", "expected": "x", "pseudo": "before"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_have_attribute", "name": "disabled"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_have_attribute", "name": "href", "expected": "/docs"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_be_in_viewport"}},
                {"action": "expect", "locator": {"by": "css", "value": "#a"}, "matcher": {"type": "to_be_in_viewport", "ratio": 0.5}}
            ]
        }))
        .unwrap();

        let observed = request
            .steps
            .iter()
            .map(|step| match step {
                BrowserStep::Expect { matcher, not, .. } => (matcher.clone(), *not),
                other => panic!("unexpected step: {other:?}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(observed[0].1, None);
        assert_eq!(observed[1].1, Some(true));
        assert_eq!(
            observed[2].0,
            BrowserExpectation::ToBeChecked {
                checked: None,
                indeterminate: None
            }
        );
        assert_eq!(
            observed[3].0,
            BrowserExpectation::ToBeChecked {
                checked: None,
                indeterminate: Some(true)
            }
        );
        assert_eq!(
            observed[4].0,
            BrowserExpectation::ToHaveCss {
                name: "color".to_string(),
                expected: BrowserExpectedText::Text("rgb(0, 0, 0)".to_string()),
                ignore_case: false,
                pseudo: None
            }
        );
        assert_eq!(
            observed[5].0,
            BrowserExpectation::ToHaveCss {
                name: "content".to_string(),
                expected: BrowserExpectedText::Text("x".to_string()),
                ignore_case: false,
                pseudo: Some(BrowserPseudoElement::Before)
            }
        );
        assert_eq!(
            observed[6].0,
            BrowserExpectation::ToHaveAttribute {
                name: "disabled".to_string(),
                expected: None,
                ignore_case: false
            }
        );
        assert_eq!(
            observed[7].0,
            BrowserExpectation::ToHaveAttribute {
                name: "href".to_string(),
                expected: Some(BrowserExpectedText::Text("/docs".to_string())),
                ignore_case: false
            }
        );
        assert_eq!(
            observed[8].0,
            BrowserExpectation::ToBeInViewport { ratio: None }
        );
        assert_eq!(
            observed[9].0,
            BrowserExpectation::ToBeInViewport { ratio: Some(0.5) }
        );

        let serialized = round_trip(serde_json::json!({
            "action": "expect",
            "locator": {"by": "css", "value": "#a"},
            "matcher": {"type": "to_have_attribute", "name": "disabled"},
            "not": true
        }));
        assert_eq!(serialized["not"], serde_json::json!(true));
        assert_eq!(
            serialized["matcher"],
            serde_json::json!({"type": "to_have_attribute", "name": "disabled", "ignore_case": false})
        );

        let without_not = round_trip(serde_json::json!({
            "action": "expect",
            "matcher": {"type": "to_be_in_viewport", "ratio": 0.25}
        }));
        assert_eq!(without_not.get("not"), None);
    }

    #[test]
    fn expect_polish_rejects_unknown_step_fields_and_bad_matchers() {
        for (step, expected) in [
            (
                serde_json::json!({"action": "expect", "matcher": {"type": "to_be_visible"}, "nott": true}),
                "nott",
            ),
            (
                serde_json::json!({"action": "expect", "matcher": {"type": "to_be_visible"}, "not": "yes"}),
                "expected a boolean",
            ),
            (
                serde_json::json!({"action": "expect", "matcher": {"type": "to_have_css", "name": "content", "expected": "x", "pseudo": "first_line"}}),
                "first_line",
            ),
            (
                serde_json::json!({"action": "expect", "matcher": {"type": "to_have_attribute"}}),
                "name",
            ),
            (
                serde_json::json!({"action": "expect", "matcher": {"type": "to_be_checkd"}}),
                "to_be_checkd",
            ),
            (serde_json::json!({"action": "expect"}), "matcher"),
        ] {
            assert_step_rejection(step, expected);
        }
    }

    #[test]
    fn lifecycle_steps_parse_with_their_defaults() {
        let request = parse_browser_action_request(serde_json::json!({
            "block_service_workers": true,
            "steps": [
                {"action": "cancel_download"},
                {"action": "cancel_download", "id": "dl-1"},
                {"action": "grant_permissions", "permissions": ["geolocation"]},
                {"action": "grant_permissions", "permissions": ["camera"], "origin": "https://example.com", "state": "denied"},
                {"action": "grant_permissions", "permissions": ["clipboard-read"], "state": "prompt"},
                {"action": "start_har_recording", "mode": "minimal", "content": "omit"},
                {"action": "start_har_recording", "mode": "full", "content": "embed", "update": "merge"},
                {"action": "storage_state"},
                {"action": "storage_state", "save_as": "state.json", "indexed_db": true},
                {"action": "set_storage_state", "state": {}},
                {"action": "set_storage_state", "state": {"cookies": [], "origins": []}, "indexed_db": false}
            ]
        }))
        .unwrap();

        assert_eq!(request.block_service_workers, Some(true));
        assert!(matches!(
            request.steps.as_slice(),
            [
                BrowserStep::CancelDownload { id: None },
                BrowserStep::CancelDownload { id: Some(_) },
                BrowserStep::GrantPermissions {
                    origin: None,
                    state: BrowserPermissionState::Granted,
                    ..
                },
                BrowserStep::GrantPermissions {
                    origin: Some(_),
                    state: BrowserPermissionState::Denied,
                    ..
                },
                BrowserStep::GrantPermissions {
                    state: BrowserPermissionState::Prompt,
                    ..
                },
                BrowserStep::StartHarRecording {
                    mode: HarMode::Minimal,
                    content: HarContentPolicy::Omit,
                    update: None,
                    ..
                },
                BrowserStep::StartHarRecording {
                    mode: HarMode::Full,
                    content: HarContentPolicy::Embed,
                    update: Some(_),
                    ..
                },
                BrowserStep::StorageState {
                    save_as: None,
                    indexed_db: None,
                },
                BrowserStep::StorageState {
                    save_as: Some(_),
                    indexed_db: Some(true),
                },
                BrowserStep::SetStorageState {
                    indexed_db: None,
                    ..
                },
                BrowserStep::SetStorageState {
                    indexed_db: Some(false),
                    ..
                },
            ]
        ));

        assert_eq!(
            round_trip(serde_json::json!({"action": "cancel_download"})),
            serde_json::json!({"action": "cancel_download"})
        );
        assert_eq!(
            round_trip(
                serde_json::json!({"action": "grant_permissions", "permissions": ["geolocation"]})
            ),
            serde_json::json!({"action": "grant_permissions", "permissions": ["geolocation"]})
        );
        assert_eq!(
            round_trip(
                serde_json::json!({"action": "grant_permissions", "permissions": ["camera"], "state": "prompt"})
            ),
            serde_json::json!({"action": "grant_permissions", "permissions": ["camera"], "state": "prompt"})
        );
        assert_eq!(
            round_trip(serde_json::json!({"action": "storage_state"})),
            serde_json::json!({"action": "storage_state"})
        );
        round_trip(serde_json::json!({
            "action": "start_har_recording",
            "mode": "full",
            "content": "attach",
            "update": "merge"
        }));
        round_trip(
            serde_json::json!({"action": "set_storage_state", "state": {}, "indexed_db": true}),
        );
    }

    #[test]
    fn lifecycle_steps_reject_unknown_and_missing_fields() {
        for (step, expected) in [
            (
                serde_json::json!({"action": "cancel_download", "download_id": "dl-1"}),
                "download_id",
            ),
            (
                serde_json::json!({"action": "grant_permissions", "permissions": ["camera"], "state": "ask"}),
                "ask",
            ),
            (
                serde_json::json!({"action": "grant_permissions", "origin": "https://x.test"}),
                "permissions",
            ),
            (
                serde_json::json!({"action": "start_har_recording", "mode": "minimal", "content": "omit", "updat": "merge"}),
                "updat",
            ),
            (
                serde_json::json!({"action": "storage_state", "indexedDb": true}),
                "indexedDb",
            ),
            (
                serde_json::json!({"action": "set_storage_state", "indexed_db": true}),
                "state",
            ),
        ] {
            assert_step_rejection(step, expected);
        }
    }

    #[test]
    fn block_service_workers_is_an_optional_batch_level_flag() {
        let omitted = parse_browser_action_request(serde_json::json!({"steps": []})).unwrap();
        assert_eq!(omitted.block_service_workers, None);
        assert_eq!(
            serde_json::to_value(&omitted)
                .unwrap()
                .get("block_service_workers"),
            None
        );

        for flag in [true, false] {
            let parsed = parse_browser_action_request(
                serde_json::json!({"block_service_workers": flag, "steps": []}),
            )
            .unwrap();
            assert_eq!(parsed.block_service_workers, Some(flag));
            assert_eq!(
                serde_json::to_value(&parsed).unwrap()["block_service_workers"],
                serde_json::json!(flag)
            );
        }

        for rejected in [
            serde_json::json!({"block_service_workers": "yes", "steps": []}),
            serde_json::json!({"blockServiceWorkers": true, "steps": []}),
        ] {
            assert!(
                parse_browser_action_request(rejected.clone()).is_err(),
                "expected rejection: {rejected}"
            );
        }
    }
}
