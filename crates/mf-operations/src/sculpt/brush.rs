//! Brushes, their settings and the sampled dab (MF-SCULPT-001,
//! MF-SCULPT-003).

use mf_geometry::FaceId;

use super::falloff::Falloff;

/// What a brush does to the vertices under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushKind {
    /// Moves each vertex toward the average of its neighbours.
    Smooth,
    /// Pushes vertices out along their normals.
    Inflate,
    /// Pulls vertices in along their normals.
    Deflate,
    /// Moves vertices onto the plane under the brush.
    Flatten,
    /// Moves vertices with the brush as it travels.
    Drag,
    /// Draws vertices in toward the brush centre, along the surface.
    Pinch,
    /// Pinches and cuts in: a sharp groove.
    Crease,
    /// Displaces inward along the surface's normal under the brush.
    Carve,
}

impl BrushKind {
    pub const ALL: [BrushKind; 8] = [
        BrushKind::Smooth,
        BrushKind::Inflate,
        BrushKind::Deflate,
        BrushKind::Flatten,
        BrushKind::Drag,
        BrushKind::Pinch,
        BrushKind::Crease,
        BrushKind::Carve,
    ];

    /// The spec's word for it.
    pub fn word(self) -> &'static str {
        match self {
            BrushKind::Smooth => "smooth",
            BrushKind::Inflate => "inflate",
            BrushKind::Deflate => "deflate",
            BrushKind::Flatten => "flatten",
            BrushKind::Drag => "drag",
            BrushKind::Pinch => "pinch",
            BrushKind::Crease => "crease",
            BrushKind::Carve => "carve",
        }
    }

    pub fn parse(word: &str) -> Option<BrushKind> {
        BrushKind::ALL.into_iter().find(|b| b.word() == word)
    }
}

/// A stroke's settings, all in the mesh's own (object) space.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushParams {
    pub kind: BrushKind,
    /// Radius of influence around each dab centre.
    pub radius: f64,
    /// 0–1: how much one dab does.
    pub strength: f64,
    pub falloff: Falloff,
    /// Mirror each dab across the plane through `pivot` normal to x, y, z.
    pub symmetry: [bool; 3],
    pub pivot: [f64; 3],
    /// When set, only vertices of these faces move (a selection).
    pub limit_to: Option<Vec<FaceId>>,
    /// Vertices of these faces never move (the freeze mask).
    pub frozen: Vec<FaceId>,
    /// The view direction the stroke was drawn along: dab centres are
    /// projected along it onto the surface as it is mid-stroke.
    pub view_dir: [f64; 3],
}

/// One dab of a stroke, in a remappable form (MF-HIST-002): a point on a
/// face of the stroke's **input** revision, never a raw position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DabSample {
    pub face: FaceId,
    pub bary: [f64; 3],
    /// 0–1 (1 where the device reports no pressure, D19).
    pub pressure: f64,
}

impl DabSample {
    /// The `[face, b0, b1, b2, pressure]` form a command records.
    pub fn to_array(&self) -> [f64; 5] {
        [
            self.face.0 as f64,
            self.bary[0],
            self.bary[1],
            self.bary[2],
            self.pressure,
        ]
    }

    pub fn from_array(a: [f64; 5]) -> DabSample {
        DabSample {
            face: FaceId(a[0] as u32),
            bary: [a[1], a[2], a[3]],
            pressure: a[4],
        }
    }
}
