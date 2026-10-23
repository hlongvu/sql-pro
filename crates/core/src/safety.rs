//! Connection-level query safety and confirmation policy.

use serde::{Deserialize, Serialize};

/// Persisted protection applied to statements before execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSafetyLevel {
    Off,
    #[default]
    WarnWrites,
    ProtectAll,
}

/// Backward-friendly short name for callers that do not need the connection
/// prefix.
pub type SafetyLevel = ConnectionSafetyLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatementRisk {
    ReadOnly,
    Write,
    Destructive,
    Unknown,
}

impl StatementRisk {
    pub fn is_write(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    pub fn is_destructive(self) -> bool {
        matches!(self, Self::Destructive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfirmationRequirement {
    None,
    Confirm,
    ConfirmDestructive,
    ConfirmProduction,
}

impl ConfirmationRequirement {
    pub fn requires_confirmation(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Context that makes the confirmation policy independent of the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmationContext<'a> {
    pub safety_level: ConnectionSafetyLevel,
    pub risk: StatementRisk,
    pub connection_tag: Option<&'a str>,
}

impl<'a> ConfirmationContext<'a> {
    pub fn decision(self) -> ConfirmationRequirement {
        confirmation_requirement(self.safety_level, self.risk, self.connection_tag)
    }
}

/// Decide how prominently a UI must confirm execution.
///
/// Destructive statements and writes on production-like connections are
/// intentionally stronger than an ordinary write. `Unknown` is never treated
/// as read-only: it follows the write path and therefore fails safe.
pub fn confirmation_requirement(
    safety_level: ConnectionSafetyLevel,
    risk: StatementRisk,
    connection_tag: Option<&str>,
) -> ConfirmationRequirement {
    if risk == StatementRisk::ReadOnly {
        return ConfirmationRequirement::None;
    }

    if is_production_tag(connection_tag) {
        return ConfirmationRequirement::ConfirmProduction;
    }

    if risk == StatementRisk::Destructive {
        return ConfirmationRequirement::ConfirmDestructive;
    }

    match safety_level {
        ConnectionSafetyLevel::Off => ConfirmationRequirement::None,
        ConnectionSafetyLevel::WarnWrites | ConnectionSafetyLevel::ProtectAll => {
            ConfirmationRequirement::Confirm
        }
    }
}

/// Recognize common environment labels without making arbitrary tags such as
/// "product-demo" production-sensitive.
pub fn is_production_tag(tag: Option<&str>) -> bool {
    let Some(tag) = tag else {
        return false;
    };
    tag.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .any(|part| {
            part.eq_ignore_ascii_case("prod")
                || part.eq_ignore_ascii_case("production")
                || part.eq_ignore_ascii_case("live")
        })
}
