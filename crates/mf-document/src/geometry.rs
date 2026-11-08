use mf_geometry::{ContentHash, Mesh};

/// An object's geometry. Meshes are the only Phase 0 kind, but no document,
/// history or command API assumes triangles (MF-DOC-005).
/// *Evolves:* `PointCloud`, `Brep`, `Volume`.
#[derive(Debug, Clone)]
pub enum Geometry {
    Mesh(Mesh),
    /// Test-only stand-in for a future non-mesh kind.
    #[cfg(feature = "test-kinds")]
    Dummy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryKind {
    Mesh,
    #[cfg(feature = "test-kinds")]
    Dummy,
}

impl GeometryKind {
    /// Name used in operation specs' `geometry_kinds`.
    pub fn as_str(&self) -> &'static str {
        match self {
            GeometryKind::Mesh => "mesh",
            #[cfg(feature = "test-kinds")]
            GeometryKind::Dummy => "dummy",
        }
    }
}

impl Geometry {
    pub fn kind(&self) -> GeometryKind {
        match self {
            Geometry::Mesh(_) => GeometryKind::Mesh,
            #[cfg(feature = "test-kinds")]
            Geometry::Dummy => GeometryKind::Dummy,
        }
    }

    pub fn as_mesh(&self) -> Option<&Mesh> {
        match self {
            Geometry::Mesh(m) => Some(m),
            #[cfg(feature = "test-kinds")]
            Geometry::Dummy => None,
        }
    }

    /// Content identity of the geometry (MF-DOC-003).
    pub fn content_hash(&self) -> ContentHash {
        match self {
            Geometry::Mesh(m) => m.content_hash(),
            #[cfg(feature = "test-kinds")]
            Geometry::Dummy => ContentHash([0; 32]),
        }
    }

    pub fn heap_bytes(&self) -> u64 {
        match self {
            Geometry::Mesh(m) => m.heap_bytes(),
            #[cfg(feature = "test-kinds")]
            Geometry::Dummy => 0,
        }
    }

    /// True if both values share the same underlying revision.
    pub fn ptr_eq(a: &Geometry, b: &Geometry) -> bool {
        match (a, b) {
            (Geometry::Mesh(x), Geometry::Mesh(y)) => Mesh::ptr_eq(x, y),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }
}
