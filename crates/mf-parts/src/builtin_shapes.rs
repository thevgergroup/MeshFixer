//! The built-in parametric shapes (`parametric-shapes-brief.md`,
//! "Library additions"), generated in code like the parts library's
//! built-ins (D16). Solids sit on z = 0 and grow along +z, centred on the
//! z axis; hole-like shapes reach down from z = 0 (their axis the local
//! z), with a little extra above it so a cut is clean.

use std::sync::OnceLock;

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};

use crate::connector::ConnectorError;
use crate::shape::{P, Shape, ShapeRole, float, int, refuse, spec_of};

/// A built-in shape: its id, name, default role, spec and generator.
struct Simple {
    id: &'static str,
    name: &'static str,
    role: ShapeRole,
    spec: fn() -> &'static OperationSpec,
    make: fn(&Params) -> Result<Mesh, ConnectorError>,
}

impl Shape for Simple {
    fn id(&self) -> &'static str {
        self.id
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn version(&self) -> u32 {
        1
    }
    fn spec(&self) -> &'static OperationSpec {
        (self.spec)()
    }
    fn role(&self) -> ShapeRole {
        self.role
    }
    fn generate(&self, params: &Params, ctx: &RunContext) -> Result<Mesh, ConnectorError> {
        ctx.checkpoint()
            .map_err(|_| refuse("cancelled", "cancelled by the user"))?;
        (self.make)(params)
    }
}

macro_rules! spec {
    ($id:literal, $summary:literal, [$($p:expr),* $(,)?]) => {{
        static S: OnceLock<OperationSpec> = OnceLock::new();
        S.get_or_init(|| spec_of($id, $summary, vec![$($p),*]))
    }};
}

const SEGMENTS: P = P::Int(
    "segments",
    48,
    3,
    512,
    "Points around each round edge (more is smoother)",
);

fn positive(name: &str, v: f64) -> Result<f64, ConnectorError> {
    if v > 0.0 && v.is_finite() {
        Ok(v)
    } else {
        Err(refuse(
            "invalid_parameter",
            format!("{name} must be more than 0 (it's {v})"),
        ))
    }
}

fn make_box(p: &Params) -> Result<Mesh, ConnectorError> {
    let size = [
        positive("width", float(p, "width")?)?,
        positive("depth", float(p, "depth")?)?,
        positive("height", float(p, "height")?)?,
    ];
    let round =
        float(p, "corner_radius")?.clamp(0.0, size.iter().copied().fold(f64::MAX, f64::min) / 2.0);
    let n = if round > 0.0 { 12 } else { 1 };
    Ok(crate::builtin::rounded_box(size, round, n))
}

fn make_cylinder(p: &Params) -> Result<Mesh, ConnectorError> {
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let h = positive("height", float(p, "height")?)?;
    let n = int(p, "segments")? as u32;
    Ok(crate::builtin::revolve(
        &[(0.0, 0.0), (r, 0.0), (r, h), (0.0, h)],
        n,
    ))
}

fn make_sphere(p: &Params) -> Result<Mesh, ConnectorError> {
    use std::f64::consts::FRAC_PI_2;
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let n = int(p, "segments")? as u32;
    let rings = (n / 2).max(2);
    Ok(crate::builtin::revolve(
        &crate::builtin::arc(r, r, -FRAC_PI_2, FRAC_PI_2, rings),
        n,
    ))
}

/// Every built-in shape, in library order.
pub fn builtin_shapes() -> Vec<&'static dyn Shape> {
    static ALL: OnceLock<Vec<Simple>> = OnceLock::new();
    ALL.get_or_init(|| {
        vec![
            Simple {
                id: "shape.box",
                name: "Box",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.box",
                        "A box, optionally with rounded corners.",
                        [
                            P::Mm("width", 20.0, 0.01, None, "Size along x"),
                            P::Mm("depth", 20.0, 0.01, None, "Size along y"),
                            P::Mm("height", 20.0, 0.01, None, "Size along z"),
                            P::Mm(
                                "corner_radius",
                                0.0,
                                0.0,
                                None,
                                "Radius of the rounded corners and edges (0: sharp)"
                            ),
                        ]
                    )
                },
                make: make_box,
            },
            Simple {
                id: "shape.cylinder",
                name: "Cylinder",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.cylinder",
                        "A round cylinder standing on its base.",
                        [
                            P::Mm("diameter", 20.0, 0.01, None, "Diameter"),
                            P::Mm("height", 20.0, 0.01, None, "Height"),
                            SEGMENTS,
                        ]
                    )
                },
                make: make_cylinder,
            },
            Simple {
                id: "shape.sphere",
                name: "Sphere",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.sphere",
                        "A sphere resting on the plate.",
                        [P::Mm("diameter", 20.0, 0.01, None, "Diameter"), SEGMENTS]
                    )
                },
                make: make_sphere,
            },
        ]
    })
    .iter()
    .map(|s| s as &dyn Shape)
    .collect()
}
