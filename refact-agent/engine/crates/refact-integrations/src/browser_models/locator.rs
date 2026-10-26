use serde::{Deserialize, Deserializer, Serialize, Serializer};
use super::helpers::is_false;

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
