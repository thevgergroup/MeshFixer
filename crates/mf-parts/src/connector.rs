//! The vocabulary connectors share (Phase 1 Task 25, MF-MODEL-005): which
//! side of a mating pair a part belongs to ([`Side`], the generator's
//! `TargetSlot`), how it joins it ([`JoinKind`]) and the refusal a
//! generator returns ([`ConnectorError`]). The generators themselves are
//! [`crate::Generator`]s.

/// Which side of the mating pair a part belongs to (D22): `A` and `B`
/// are the two things the connector joins (a body and a lid; two halves
/// of a box); `Neither` is a part that's temporary or stands alone (not
/// unioned into either side by `connector.join`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    A,
    B,
    Neither,
}

/// How a part joins its side, in `connector.join` (Task 28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Union,
    Subtract,
    /// Becomes its own new object instead of joining a side (a loose
    /// pin, a separate printed piece).
    Separate,
}

/// An error generating a connector (refused by name, D4: never a plain
/// panic or a silently wrong part).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConnectorError {
    #[error("{code}: {message}")]
    Failed { code: &'static str, message: String },
}
