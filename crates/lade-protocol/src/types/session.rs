//! Agent Session vocabulary: models, thinking levels, delivery modes, the session tree and
//! extension UI prompts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityRole {
    User,
    Assistant,
    Thinking,
    Tool,
    System,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == text)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeEntryKind {
    User,
    Assistant,
    ToolResult,
    CustomMessage,
    Compaction,
    BranchSummary,
    ModelChange,
    ThinkingChange,
    Label,
    SessionInfo,
    Custom,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub entry_id: String,
    pub kind: TreeEntryKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiPrompt {
    Confirm { title: String, message: String },
    Select { title: String, options: Vec<String> },
    Input { title: String, placeholder: String },
    Editor { title: String, prefill: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiAnswer {
    Confirm { confirmed: bool },
    Value { value: String },
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCloseReason {
    Timeout,
    Aborted,
    Replaced,
    Answered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionChangeReason {
    Start,
    Navigate,
    Fork,
    Import,
    Reload,
    Compact,
    Configure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPosition {
    Before,
    At,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Contract, client_enum, conforms};
    use serde_json::{Value, json};

    fn wire<T: Serialize>(values: &[T]) -> Value {
        serde_json::to_value(values).expect("serialize")
    }

    #[test]
    fn session_vocabularies_match_the_contract_exactly() {
        use ActivityRole as R;
        use SessionChangeReason as C;
        use TreeEntryKind as K;
        assert_eq!(
            wire(&ThinkingLevel::ALL),
            client_enum("/$defs/ThinkingLevel")
        );
        assert_eq!(
            wire(&[
                DeliveryMode::Prompt,
                DeliveryMode::Steer,
                DeliveryMode::FollowUp
            ]),
            client_enum("/$defs/DeliveryMode")
        );
        assert_eq!(
            wire(&[R::User, R::Assistant, R::Thinking, R::Tool, R::System]),
            client_enum("/$defs/ActivityRole")
        );
        let kinds = [
            K::User,
            K::Assistant,
            K::ToolResult,
            K::CustomMessage,
            K::Compaction,
            K::BranchSummary,
            K::ModelChange,
            K::ThinkingChange,
            K::Label,
            K::SessionInfo,
            K::Custom,
            K::Other,
        ];
        assert_eq!(
            wire(&kinds),
            client_enum("/$defs/TreeEntry/properties/kind")
        );
        let closes = [
            UiCloseReason::Timeout,
            UiCloseReason::Aborted,
            UiCloseReason::Replaced,
            UiCloseReason::Answered,
        ];
        assert_eq!(wire(&closes), client_enum("/$defs/UiCloseReason"));
        let changes = [
            C::Start,
            C::Navigate,
            C::Fork,
            C::Import,
            C::Reload,
            C::Compact,
            C::Configure,
        ];
        assert_eq!(wire(&changes), client_enum("/$defs/SessionChangeReason"));
        assert_eq!(
            wire(&[ForkPosition::Before, ForkPosition::At]),
            client_enum("/$defs/SessionForkCommand/properties/position")
        );
    }

    #[test]
    fn thinking_levels_parse_exactly_their_wire_names() {
        for level in ThinkingLevel::ALL {
            assert_eq!(ThinkingLevel::parse(level.as_str()), Some(level));
            assert_eq!(
                serde_json::to_value(level).expect("serialize"),
                Value::from(level.as_str())
            );
        }
        for text in ["", "High", "x-high", "maximum"] {
            assert_eq!(ThinkingLevel::parse(text), None, "{text}");
        }
    }

    #[test]
    fn prompts_answers_and_tree_entries_take_the_contract_shapes() {
        let prompts = [
            UiPrompt::Confirm {
                title: "Proceed?".into(),
                message: "It writes files.".into(),
            },
            UiPrompt::Select {
                title: "Pick".into(),
                options: vec!["a".into(), "b".into()],
            },
            UiPrompt::Input {
                title: "Name".into(),
                placeholder: String::new(),
            },
            UiPrompt::Editor {
                title: "Edit".into(),
                prefill: "text".into(),
            },
        ];
        for prompt in &prompts {
            let json = serde_json::to_value(prompt).expect("serialize");
            assert!(
                conforms(&json, Contract::Client, "/$defs/UiPrompt"),
                "{json}"
            );
        }
        assert_eq!(
            serde_json::to_value(&prompts[1]).expect("serialize"),
            json!({"kind": "select", "title": "Pick", "options": ["a", "b"]})
        );
        for answer in [
            UiAnswer::Confirm { confirmed: true },
            UiAnswer::Value { value: "x".into() },
            UiAnswer::Cancel,
        ] {
            let json = serde_json::to_value(&answer).expect("serialize");
            assert!(
                conforms(&json, Contract::Client, "/$defs/UiAnswer"),
                "{json}"
            );
        }
        let root = TreeEntry {
            entry_id: "e1".into(),
            kind: TreeEntryKind::User,
            label: "hello".into(),
            parent_id: None,
            bookmark: None,
        };
        let json = serde_json::to_value(&root).expect("serialize");
        assert_eq!(
            json,
            json!({"entry_id": "e1", "kind": "user", "label": "hello"})
        );
        assert!(conforms(&json, Contract::Client, "/$defs/TreeEntry"));
    }

    #[test]
    fn closed_shapes_refuse_unknown_fields_and_kinds() {
        assert!(
            serde_json::from_value::<Model>(json!({"provider": "p", "id": "m", "extra": 1}))
                .is_err()
        );
        assert!(serde_json::from_value::<UiAnswer>(json!({"kind": "skip"})).is_err());
        // serde ignores extra fields on a tagged unit variant; the contract refuses them first.
        assert!(!conforms(
            &json!({"kind": "cancel", "value": "x"}),
            Contract::Client,
            "/$defs/UiAnswer"
        ));
        assert_eq!(
            serde_json::from_value::<DeliveryMode>(json!("follow_up")).ok(),
            Some(DeliveryMode::FollowUp)
        );
    }
}
