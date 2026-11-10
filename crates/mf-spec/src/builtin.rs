//! Built-in operation specs, compiled into the binary (OD-4).

use std::sync::OnceLock;

use crate::model::OperationSpec;

const SOURCES: &[(&str, &str)] = &[
    (
        "mesh.inspect.yaml",
        include_str!("../../../specs/operations/mesh.inspect.yaml"),
    ),
    (
        "mesh.fill_holes.yaml",
        include_str!("../../../specs/operations/mesh.fill_holes.yaml"),
    ),
    (
        "mesh.remove_small_components.yaml",
        include_str!("../../../specs/operations/mesh.remove_small_components.yaml"),
    ),
    (
        "mesh.compact.yaml",
        include_str!("../../../specs/operations/mesh.compact.yaml"),
    ),
    (
        "object.set_transform.yaml",
        include_str!("../../../specs/operations/object.set_transform.yaml"),
    ),
    (
        "object.drop_to_plate.yaml",
        include_str!("../../../specs/operations/object.drop_to_plate.yaml"),
    ),
    (
        "mesh.boolean.yaml",
        include_str!("../../../specs/operations/mesh.boolean.yaml"),
    ),
    (
        "object.delete.yaml",
        include_str!("../../../specs/operations/object.delete.yaml"),
    ),
    #[cfg(feature = "test-kinds")]
    (
        "test.consume_tool.yaml",
        include_str!("../../../specs/test-operations/test.consume_tool.yaml"),
    ),
    #[cfg(feature = "test-kinds")]
    (
        "test.unit_conversion.yaml",
        include_str!("../../../specs/test-operations/test.unit_conversion.yaml"),
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
