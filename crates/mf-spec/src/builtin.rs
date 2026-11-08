//! Built-in operation specs, compiled into the binary (OD-4).

use std::sync::OnceLock;

use crate::model::OperationSpec;

const SOURCES: &[(&str, &str)] = &[
    (
        "mesh.inspect.yaml",
        include_str!("../../../specs/operations/mesh.inspect.yaml"),
    ),
    (
        "mesh.remove_small_components.yaml",
        include_str!("../../../specs/operations/mesh.remove_small_components.yaml"),
    ),
];

/// Raw YAML sources, for schema validation tests.
pub fn sources() -> &'static [(&'static str, &'static str)] {
    SOURCES
}

/// All built-in specs. A malformed built-in spec is a build defect, caught by
/// the test suite, so this panics rather than limping on without it.
pub fn specs() -> &'static [OperationSpec] {
    static SPECS: OnceLock<Vec<OperationSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(name, text)| {
                serde_saphyr::from_str(text)
                    .unwrap_or_else(|e| panic!("built-in spec {name} is invalid: {e}"))
            })
            .collect()
    })
}

pub fn get(id: &str) -> Option<&'static OperationSpec> {
    specs().iter().find(|s| s.id == id)
}
