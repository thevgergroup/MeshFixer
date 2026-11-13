//! Test-only (`test-kinds` feature): `test.key`, a square key across a
//! cut. It exists to prove the generator architecture's promise
//! (parts-architecture-review.md): a new connector is this file, its
//! spec YAML and one registry entry — and with nothing else it is
//! offered in Connect, placed, applied, previewed and checked like any
//! built-in.

use std::sync::OnceLock;

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};

use crate::connector::{ConnectorError, Side};
use crate::generator::{
    Anchor, Fix, GenCheck, GenContext, Generator, Handles, ParamPatch, PlacementHint, ShapePart,
};
use crate::shape::ShapeRole;

/// Thinner than this, a printed key snaps: the key's own check.
pub const MIN_WIDTH: f64 = 2.0;

fn float(params: &Params, name: &str) -> Result<f64, ConnectorError> {
    params.get_float(name).map_err(|e| ConnectorError::Failed {
        code: "invalid_parameter",
        message: e.to_string(),
    })
}

/// A closed axis-aligned box from `lo` to `hi`.
fn cuboid(lo: [f64; 3], hi: [f64; 3]) -> Mesh {
    let p = |i: usize| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    let positions = (0..8).map(p).collect();
    let triangles = vec![
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];
    Mesh::new(positions, triangles).expect("a box is a valid mesh")
}

pub struct TestKey;

impl Generator for TestKey {
    fn id(&self) -> &'static str {
        "test.key"
    }

    fn name(&self) -> &'static str {
        "Test key"
    }

    fn version(&self) -> u32 {
        1
    }

    fn spec(&self) -> &'static OperationSpec {
        static SPEC: OnceLock<OperationSpec> = OnceLock::new();
        SPEC.get_or_init(|| {
            serde_saphyr::from_str(include_str!("../../../../specs/connectors/test.key.yaml"))
                .expect("connectors/test.key.yaml is a valid spec")
        })
    }

    fn generate(
        &self,
        params: &Params,
        _ctx: &GenContext,
    ) -> Result<Vec<ShapePart>, ConnectorError> {
        let w = float(params, "width")?;
        let c = float(params, "clearance")?;
        let (on, other) = (float(params, "depth_on")?, float(params, "depth_other")?);
        let gap = float(params, "gap")?;
        if c >= w {
            return Err(ConnectorError::Failed {
                code: "clearance_too_wide",
                message: format!("The clearance ({c:.2} mm) is as wide as the key ({w:.2} mm)."),
            });
        }
        let (a, b) = (-gap / 2.0, gap / 2.0);
        let k = (w - c) / 2.0;
        let s = w / 2.0;
        Ok(vec![
            ShapePart::new(
                "key",
                ShapeRole::Solid,
                Side::A,
                cuboid([-k, -k, a - on], [k, k, b + other - c.max(0.1)]),
            ),
            ShapePart::new(
                "socket",
                ShapeRole::Hole,
                Side::B,
                cuboid([-s, -s, b - w], [s, s, b + other]),
            ),
        ])
    }

    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Point
    }

    fn checks(&self, params: &Params, _ctx: &GenContext) -> Vec<GenCheck> {
        match params.get_float("width") {
            Ok(w) if w < MIN_WIDTH => vec![GenCheck {
                id: "key_too_thin".into(),
                message: format!(
                    "The key ({w:.1} mm) is thinner than {MIN_WIDTH:.1} mm and may snap."
                ),
                fix: Some(Fix {
                    label: "Widen the key".into(),
                    patch: ParamPatch::float("width", MIN_WIDTH),
                }),
            }],
            _ => Vec::new(),
        }
    }

    fn anchor(&self, _params: &Params) -> Anchor {
        // Square: it resists turning on its own.
        Anchor::Resists
    }

    fn handles(&self) -> Handles {
        Handles {
            size: Some("width"),
            depth: Some("depth_on"),
        }
    }
}
