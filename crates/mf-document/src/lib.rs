//! Document state: objects and their geometry (MF-DOC-001…005).
//!
//! Documents are immutable values. `with_*` methods return a new document
//! and never change the original, so the only way to change *session*
//! state is through `mf-command` (MF-CMD-001).

mod geometry;

use mf_io::SourceInfo;

pub use geometry::{Geometry, GeometryKind};

/// Stable for the lifetime of a session. Never reused (MF-DOC-004).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ObjectId(pub u64);

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Object {
    pub id: ObjectId,
    pub name: String,
    pub geometry: Geometry,
    pub source: Option<SourceInfo>,
}

impl Object {
    /// The mesh, if this object's geometry is a mesh.
    pub fn mesh(&self) -> Option<&mf_geometry::Mesh> {
        self.geometry.as_mesh()
    }
}

/// An immutable document state. Cloning is cheap because geometry is shared.
#[derive(Debug, Clone, Default)]
pub struct Document {
    objects: Vec<Object>,
}

impl Document {
    pub fn objects(&self) -> &[Object] {
        &self.objects
    }

    pub fn object(&self, id: ObjectId) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    /// A new document with `object` appended.
    pub fn with_object(&self, object: Object) -> Document {
        let mut d = self.clone();
        d.objects.push(object);
        d
    }

    /// A new document with one object's geometry replaced, or `None` if the
    /// object doesn't exist.
    pub fn with_geometry(&self, id: ObjectId, geometry: Geometry) -> Option<Document> {
        let mut d = self.clone();
        d.objects.iter_mut().find(|o| o.id == id)?.geometry = geometry;
        Some(d)
    }
}
