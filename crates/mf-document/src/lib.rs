//! Document state: objects and their geometry (MF-DOC-001…005).
//!
//! Documents are immutable values. `with_*` methods return a new document
//! and never change the original, so the only way to change *session*
//! state is through `mf-command` (MF-CMD-001).

mod geometry;

use std::collections::BTreeMap;

use mf_io::SourceInfo;
pub use mf_io::threemf::{IDENTITY as IDENTITY_TRANSFORM, Unit};

pub use geometry::{Geometry, GeometryKind};

/// A document state in a session's revision DAG (MF-HIST-001). Monotonic,
/// never reused.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct RevisionId(pub u64);

impl std::fmt::Display for RevisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

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
    /// Placement as a row-major 3×4 affine in 3MF order (last three values
    /// are the translation). Stored and exported; Phase 0 tools edit geometry
    /// in object space and the viewer draws the active object untransformed.
    pub transform: [f64; 12],
    /// Holes marked as intentional openings (M4 Task 12): boundary-loop
    /// ids of this revision's mesh, sorted. Commands that change the mesh
    /// translate them to the new revision's ids.
    pub openings: Vec<u32>,
    /// Object properties (key to value), such as its Solid/Hole role
    /// ([`ROLE`]). Saved in 3MF as `meshfixer:<key>` object metadata
    /// ([`Object::metadata`]) and read back on import; changed only by an
    /// undoable history step (`Session::set_property`). Geometry commands
    /// keep an object's properties; objects they create have none.
    pub properties: BTreeMap<String, String>,
}

/// The property naming an object's role in a combine: [`HOLE`], or absent
/// for a solid (the default).
pub const ROLE: &str = "role";
/// The [`ROLE`] value of a hole: subtracted when combined.
pub const HOLE: &str = "hole";
/// The property holding a parametric shape's recipe (its generator id,
/// version and params, as JSON; `mf_parts::GeneratorRecord`): present
/// while the object is an editable shape, removed when a mesh operation
/// bakes it into a plain mesh (parametric-shapes brief).
pub const GENERATOR: &str = "generator";
/// The property naming the linked group a parametric shape belongs to (a
/// connector placed as several objects: a peg and its hole, a hinge's
/// leaves and pin). Editing the group's params regenerates every member.
pub const GROUP: &str = "group";
/// The properties a mesh operation removes when it changes an object's
/// geometry: what made it an editable shape (it is a plain mesh after).
pub const PARAMETRIC_KEYS: [&str; 2] = [GENERATOR, GROUP];
/// The 3MF metadata prefix of an object property.
const METADATA_PREFIX: &str = "meshfixer:";

/// The object properties in 3MF object metadata (the `meshfixer:` entries,
/// prefix removed); other entries are left alone.
pub fn properties_from_metadata(metadata: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix(METADATA_PREFIX)?.to_owned(), v.clone())))
        .collect()
}

impl Object {
    /// True if the object's role is [`HOLE`].
    pub fn is_hole(&self) -> bool {
        self.properties.get(ROLE).is_some_and(|r| r == HOLE)
    }

    /// True while the object is an editable shape (it has a recipe).
    pub fn is_parametric(&self) -> bool {
        self.properties.contains_key(GENERATOR)
    }

    /// The object's properties as 3MF object metadata (`meshfixer:<key>`).
    pub fn metadata(&self) -> BTreeMap<String, String> {
        self.properties
            .iter()
            .map(|(k, v)| (format!("{METADATA_PREFIX}{k}"), v.clone()))
            .collect()
    }

    /// The mesh, if this object's geometry is a mesh.
    pub fn mesh(&self) -> Option<&mf_geometry::Mesh> {
        self.geometry.as_mesh()
    }
}

/// An immutable document state. Cloning is cheap because geometry is shared.
#[derive(Debug, Clone, Default)]
pub struct Document {
    objects: Vec<Object>,
    /// Model units (MF-DOC-004); millimetres unless an import says otherwise.
    units: Unit,
}

impl Document {
    pub fn units(&self) -> Unit {
        self.units
    }

    /// A new document with different units (coordinates are not converted).
    pub fn with_units(&self, units: Unit) -> Document {
        let mut d = self.clone();
        d.units = units;
        d
    }

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

    /// A new document with one object's intentional openings replaced
    /// (sorted), or `None` if the object doesn't exist.
    pub fn with_openings(&self, id: ObjectId, mut openings: Vec<u32>) -> Option<Document> {
        openings.sort_unstable();
        openings.dedup();
        let mut d = self.clone();
        d.objects.iter_mut().find(|o| o.id == id)?.openings = openings;
        Some(d)
    }

    /// A new document with one object's property `key` set to `value`
    /// (`None` removes it), or `None` if the object doesn't exist.
    pub fn with_property(
        &self,
        id: ObjectId,
        key: &str,
        value: Option<String>,
    ) -> Option<Document> {
        let mut d = self.clone();
        let props = &mut d.objects.iter_mut().find(|o| o.id == id)?.properties;
        match value {
            Some(v) => props.insert(key.to_owned(), v),
            None => props.remove(key),
        };
        Some(d)
    }

    /// A new document with one object's geometry replaced, or `None` if the
    /// object doesn't exist.
    pub fn with_geometry(&self, id: ObjectId, geometry: Geometry) -> Option<Document> {
        let mut d = self.clone();
        d.objects.iter_mut().find(|o| o.id == id)?.geometry = geometry;
        Some(d)
    }

    /// A new document with one object's placement replaced (the absolute
    /// 3MF row-major 3×4 affine, D7), or `None` if the object doesn't
    /// exist. The geometry is shared, not copied.
    pub fn with_transform(&self, id: ObjectId, transform: [f64; 12]) -> Option<Document> {
        let mut d = self.clone();
        d.objects.iter_mut().find(|o| o.id == id)?.transform = transform;
        Some(d)
    }

    /// A new document with one object renamed, or `None` if it doesn't
    /// exist.
    pub fn with_name(&self, id: ObjectId, name: String) -> Option<Document> {
        let mut d = self.clone();
        d.objects.iter_mut().find(|o| o.id == id)?.name = name;
        Some(d)
    }

    /// A new document without one object, or `None` if it doesn't exist.
    /// The original document still holds it, so undo restores it with the
    /// same id, in the same place.
    pub fn without_object(&self, id: ObjectId) -> Option<Document> {
        let at = self.objects.iter().position(|o| o.id == id)?;
        let mut d = self.clone();
        d.objects.remove(at);
        Some(d)
    }

    /// A new document with a newly created object appended, or `None` if
    /// an object with its id already exists (ids are never reused,
    /// MF-DOC-004).
    pub fn with_new_object(&self, object: Object) -> Option<Document> {
        if self.object(object.id).is_some() {
            return None;
        }
        Some(self.with_object(object))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure

    use super::*;

    fn object(id: u64) -> Object {
        let mesh = mf_geometry::Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
        .unwrap();
        Object {
            id: ObjectId(id),
            name: format!("o{id}"),
            geometry: Geometry::Mesh(mesh),
            source: None,
            transform: IDENTITY_TRANSFORM,
            openings: Vec::new(),
            properties: BTreeMap::new(),
        }
    }

    #[test]
    fn object_changes_leave_the_original_document_alone() {
        let doc = Document::default()
            .with_object(object(1))
            .with_object(object(2));
        let mut t = IDENTITY_TRANSFORM;
        t[11] = 5.0;
        let moved = doc.with_transform(ObjectId(1), t).unwrap();
        assert_eq!(moved.object(ObjectId(1)).unwrap().transform, t);
        assert_eq!(
            doc.object(ObjectId(1)).unwrap().transform,
            IDENTITY_TRANSFORM
        );
        assert!(Geometry::ptr_eq(
            &moved.object(ObjectId(1)).unwrap().geometry,
            &doc.object(ObjectId(1)).unwrap().geometry
        ));
        assert!(doc.with_transform(ObjectId(9), t).is_none());

        let without = doc.without_object(ObjectId(1)).unwrap();
        assert!(without.object(ObjectId(1)).is_none());
        assert!(doc.object(ObjectId(1)).is_some());
        assert!(doc.without_object(ObjectId(9)).is_none());

        let added = without.with_new_object(object(3)).unwrap();
        assert_eq!(
            added.objects().iter().map(|o| o.id).collect::<Vec<_>>(),
            vec![ObjectId(2), ObjectId(3)]
        );
        assert!(
            added.with_new_object(object(2)).is_none(),
            "ids are never reused"
        );
    }
}
