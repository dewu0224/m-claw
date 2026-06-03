//! Channel-related types shared across the workspace.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies the type of messaging channel.
///
/// This is the canonical definition used by config, channels, and gateway crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    /// Feishu / Lark (飞书)
    #[serde(alias = "Feishu")]
    Feishu,
    /// WeChat Work (企业微信)
    #[serde(alias = "WeChat")]
    WeChat,
    /// QQ (via go-cqhttp / Lagrange.OneBot)
    #[serde(alias = "QQ")]
    QQ,
}

impl fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Feishu => write!(f, "feishu"),
            Self::WeChat => write!(f, "wechat"),
            Self::QQ => write!(f, "qq"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_lowercase() {
        assert_eq!(ChannelKind::Feishu.to_string(), "feishu");
        assert_eq!(ChannelKind::WeChat.to_string(), "wechat");
        assert_eq!(ChannelKind::QQ.to_string(), "qq");
    }

    #[test]
    fn serde_lowercase_roundtrip() {
        let kinds = [ChannelKind::Feishu, ChannelKind::WeChat, ChannelKind::QQ];
        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: ChannelKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn serde_accepts_pascal_case_alias() {
        let kind: ChannelKind = serde_json::from_str(r#""Feishu""#).unwrap();
        assert_eq!(kind, ChannelKind::Feishu);
        let kind: ChannelKind = serde_json::from_str(r#""WeChat""#).unwrap();
        assert_eq!(kind, ChannelKind::WeChat);
    }

    #[test]
    fn serde_json_values() {
        assert_eq!(serde_json::to_string(&ChannelKind::Feishu).unwrap(), "\"feishu\"");
        assert_eq!(serde_json::to_string(&ChannelKind::WeChat).unwrap(), "\"wechat\"");
        assert_eq!(serde_json::to_string(&ChannelKind::QQ).unwrap(), "\"qq\"");
    }

    #[test]
    fn equality_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ChannelKind::Feishu);
        set.insert(ChannelKind::Feishu);
        set.insert(ChannelKind::WeChat);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&ChannelKind::Feishu));
    }
}
