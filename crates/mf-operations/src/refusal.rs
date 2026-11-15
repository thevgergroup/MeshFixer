//! The refusal registry (ADR-0010, consolidation plan C14): every
//! [`OpError::Refused`] is built through [`refuse`], the one place that
//! turns a code and its typed arguments into an [`OpError`]. A length
//! argument is [`mf_units::Length`] (a value with its own unit), never a
//! pre-formatted string with a hard-coded `"mm"` or a bare `{:.3}`
//! number — `mf_units::format_length` renders it later, at the point
//! that knows which unit to show (the desktop's `RefusalCard`, C16; the
//! CLI's own text, below).
//!
//! `OpError::Failed` still exists, for a genuine internal error (a
//! malformed parameter, a cancellation, a kernel bug) that isn't one of
//! an operation's declared refusals. The lint in
//! `tests/refusal_registry.rs` checks that every `OpError::Failed` site
//! outside this module is on its reviewed allow-list.

use std::collections::BTreeMap;
use std::fmt;

use mf_units::Length;

use crate::OpError;

/// One refusal's named argument, typed so a length is never a bare
/// number or a pre-formatted string (ADR-0006 rule 1, ADR-0010).
#[derive(Debug, Clone, PartialEq)]
pub enum RefusalArg {
    Length(Length),
    Count(u64),
    Text(String),
}

impl fmt::Display for RefusalArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // No catalogue/locale here (mf-operations has no i18n
            // dependency, by design: rendering for a person is the
            // desktop's RefusalCard, C16). This is the CLI's and the
            // generic fallback's plain-English text, through the same
            // formatter every length in the app uses, so a length never
            // prints as a bare number or a hard-coded "mm".
            RefusalArg::Length(l) => write!(f, "{}", mf_units::format_length(l.value, l.unit)),
            RefusalArg::Count(n) => write!(f, "{n}"),
            RefusalArg::Text(s) => write!(f, "{s}"),
        }
    }
}

/// Builds an [`OpError::Refused`] for `code`, with its named arguments.
/// The one place every op's refusal helper constructs one (the 9
/// per-module helpers plus `leaf_hinge`'s, consolidation plan C14): the
/// lint in `tests/refusal_registry.rs` checks that nothing else builds
/// `OpError::Failed` for what is really a refusal.
pub fn refuse(code: &'static str, args: BTreeMap<&'static str, RefusalArg>) -> OpError {
    OpError::Refused { code, args }
}

/// As [`refuse`], for a refusal with no arguments.
pub fn refuse_plain(code: &'static str) -> OpError {
    refuse(code, BTreeMap::new())
}

/// A plain-English rendering of a refusal's arguments, `"name: value,
/// name: value"` — used by [`OpError`]'s `Display` (the CLI's and the
/// generic fallback's text) and by tests. Sorted by name ([`BTreeMap`]'s
/// own order), so it's deterministic.
pub fn render_args(args: &BTreeMap<&'static str, RefusalArg>) -> String {
    args.iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join(", ")
}
