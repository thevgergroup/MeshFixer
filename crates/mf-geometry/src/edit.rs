//! Copy-on-write mesh edits that record provenance as they go (MF-SEL-002).

use crate::chunked::{Bits, Chunked};
use crate::ids::{FaceId, Provenance, Sources, VertexId};
use crate::mesh::{GeometryError, Mesh};

/// Builder for a new revision of a mesh. Untouched chunks stay shared with
/// the base; kept faces keep their IDs. Every change is recorded, so the
/// resulting [`Provenance`] is total by construction.
pub struct MeshEdit {
    base_faces: u32,
    base_vertices: u32,
    positions: Chunked<[f64; 3]>,
    faces: Chunked<[u32; 3]>,
    face_live: Bits,
    vertex_live: Bits,
    face_count: usize,
    vertex_count: usize,
    prov: Provenance,
    /// Any vertex removed in this edit, including one it created. Drives the
    /// referential-integrity check in `finish` (provenance alone misses
    /// created-then-removed vertices).
    vertex_removed: bool,
}

impl MeshEdit {
    pub(crate) fn new(base: &Mesh) -> MeshEdit {
        let inner = base.inner();
        MeshEdit {
            base_faces: base.face_slots(),
            base_vertices: base.vertex_slots(),
            positions: inner.positions.clone(),
            faces: inner.faces.clone(),
            face_live: inner.face_live.clone(),
            vertex_live: inner.vertex_live.clone(),
            face_count: inner.face_count,
            vertex_count: inner.vertex_count,
            prov: Provenance::default(),
            vertex_removed: false,
        }
    }

    pub fn is_face_live(&self, id: FaceId) -> bool {
        self.face_live.get(id.0 as usize)
    }

    pub fn is_vertex_live(&self, id: VertexId) -> bool {
        self.vertex_live.get(id.0 as usize)
    }

    pub fn remove_face(&mut self, id: FaceId) -> Result<(), GeometryError> {
        if !self.is_face_live(id) {
            return Err(GeometryError::NoSuchFace(id.0));
        }
        self.face_live.set(id.0 as usize, false);
        self.face_count -= 1;
        // Faces created in this edit are dropped from `created_faces` in
        // `finish` (by liveness), keeping this O(1).
        if id.0 < self.base_faces {
            self.prov.removed_faces.push(id);
        }
        Ok(())
    }

    /// Remove a vertex. [`MeshEdit::finish`] fails if a live face still uses it.
    pub fn remove_vertex(&mut self, id: VertexId) -> Result<(), GeometryError> {
        if !self.is_vertex_live(id) {
            return Err(GeometryError::NoSuchVertex(id.0));
        }
        self.vertex_live.set(id.0 as usize, false);
        self.vertex_count -= 1;
        self.vertex_removed = true;
        if id.0 < self.base_vertices {
            self.prov.removed_vertices.push(id);
        }
        Ok(())
    }

    pub fn add_vertex(&mut self, p: [f64; 3]) -> VertexId {
        let id = VertexId(self.positions.len() as u32);
        self.positions.push(p);
        self.vertex_live.push(true);
        self.vertex_count += 1;
        self.prov.created_vertices.push(id);
        id
    }

    pub fn add_face(&mut self, tri: [u32; 3], sources: Sources) -> Result<FaceId, GeometryError> {
        if let Some(&v) = tri.iter().find(|&&v| !self.is_vertex_live(VertexId(v))) {
            return Err(GeometryError::NoSuchVertex(v));
        }
        let id = FaceId(self.faces.len() as u32);
        self.faces.push(tri);
        self.face_live.push(true);
        self.face_count += 1;
        self.prov.created_faces.push((id, sources));
        Ok(id)
    }

    /// Change a live face's vertices, keeping its ID (edge flips, welds).
    pub fn set_face(&mut self, id: FaceId, tri: [u32; 3]) -> Result<(), GeometryError> {
        if !self.is_face_live(id) {
            return Err(GeometryError::NoSuchFace(id.0));
        }
        if let Some(&v) = tri.iter().find(|&&v| !self.is_vertex_live(VertexId(v))) {
            return Err(GeometryError::NoSuchVertex(v));
        }
        self.faces.set(id.0 as usize, tri);
        if id.0 < self.base_faces {
            self.prov.modified_faces.push(id);
        }
        Ok(())
    }

    pub fn move_vertex(&mut self, id: VertexId, p: [f64; 3]) -> Result<(), GeometryError> {
        if !self.is_vertex_live(id) {
            return Err(GeometryError::NoSuchVertex(id.0));
        }
        self.positions.set(id.0 as usize, p);
        if id.0 < self.base_vertices {
            // Deduplicated in `finish`.
            self.prov.moved_vertices.push(id);
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<(Mesh, Provenance), GeometryError> {
        Mesh::check_counts(self.positions.len(), self.faces.len())?;
        let touched = self
            .prov
            .created_vertices
            .iter()
            .chain(&self.prov.moved_vertices);
        for v in touched {
            if !self
                .positions
                .get(v.0 as usize)
                .iter()
                .all(|c| c.is_finite())
            {
                return Err(GeometryError::NonFinite {
                    vertex: v.0 as usize,
                });
            }
        }
        if self.vertex_removed {
            // Linear check, only when vertices were removed.
            for (i, tri) in self.faces.iter().enumerate() {
                if !self.face_live.get(i) {
                    continue;
                }
                if let Some(&v) = tri.iter().find(|&&v| !self.vertex_live.get(v as usize)) {
                    return Err(GeometryError::FaceUsesRemovedVertex {
                        face: i as u32,
                        vertex: v,
                    });
                }
            }
        }
        // Normalise once: sorted, unique, and only elements still live
        // (created-then-removed elements never existed as far as provenance
        // is concerned; a moved-then-removed vertex is just removed).
        let face_live = &self.face_live;
        let vertex_live = &self.vertex_live;
        self.prov.removed_faces.sort_unstable();
        self.prov.removed_vertices.sort_unstable();
        self.prov
            .modified_faces
            .retain(|f| face_live.get(f.0 as usize));
        self.prov.modified_faces.sort_unstable();
        self.prov.modified_faces.dedup();
        self.prov
            .created_faces
            .retain(|(f, _)| face_live.get(f.0 as usize));
        self.prov.created_faces.sort_by_key(|(f, _)| *f);
        self.prov
            .created_vertices
            .retain(|v| vertex_live.get(v.0 as usize));
        self.prov.created_vertices.sort_unstable();
        self.prov
            .moved_vertices
            .retain(|v| vertex_live.get(v.0 as usize));
        self.prov.moved_vertices.sort_unstable();
        self.prov.moved_vertices.dedup();
        let mesh = Mesh::from_parts(
            self.positions,
            self.faces,
            self.face_live,
            self.vertex_live,
            self.face_count,
            self.vertex_count,
        );
        Ok((mesh, self.prov))
    }
}
