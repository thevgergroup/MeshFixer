use mf_geometry::Mesh;
use mf_io::SourceInfo;

/// Stable for the lifetime of a session. Never reused (MF-DOC-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    pub mesh: Mesh,
    pub source: Option<SourceInfo>,
}

/// An immutable document state. Cloning is cheap because meshes are shared.
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

    pub(crate) fn with_object(&self, object: Object) -> Document {
        let mut d = self.clone();
        d.objects.push(object);
        d
    }

    pub(crate) fn with_mesh(&self, id: ObjectId, mesh: Mesh) -> Option<Document> {
        let mut d = self.clone();
        d.objects.iter_mut().find(|o| o.id == id)?.mesh = mesh;
        Some(d)
    }
}
