use mf_geometry::Mesh;
use mf_spec::Params;
use serde_json::json;

use crate::{OpError, OpOutput, Operation};

pub struct RemoveSmallComponents;

impl Operation for RemoveSmallComponents {
    fn id(&self) -> &'static str {
        "mesh.remove_small_components"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(&self, input: &Mesh, params: &Params) -> Result<OpOutput, OpError> {
        let max_triangles = params.get_int("max_triangles")?.max(0) as usize;
        let keep_largest = params.get_bool("keep_largest")?;
        let topo = input.topology();

        let mut keep_face = vec![false; input.triangle_count()];
        let mut removed_components = 0;
        let mut removed_triangles = 0;
        // Components are ordered largest first, so index 0 is the largest.
        for (i, comp) in topo.components.iter().enumerate() {
            if comp.len() >= max_triangles || (keep_largest && i == 0) {
                for &f in comp {
                    keep_face[f as usize] = true;
                }
            } else {
                removed_components += 1;
                removed_triangles += comp.len();
            }
        }

        // Compact, keeping the original relative order of both kept faces
        // and kept vertices (MF-SEL-001, MF-OP-003).
        let mut used = vec![false; input.vertex_count()];
        for (f, t) in input.triangles().iter().enumerate() {
            if keep_face[f] {
                for &v in t {
                    used[v as usize] = true;
                }
            }
        }
        let mut remap = vec![u32::MAX; input.vertex_count()];
        let mut positions = Vec::new();
        for (v, p) in input.positions().iter().enumerate() {
            if used[v] {
                remap[v] = positions.len() as u32;
                positions.push(*p);
            }
        }
        let triangles = input
            .triangles()
            .iter()
            .enumerate()
            .filter(|(f, _)| keep_face[*f])
            .map(|(_, t)| t.map(|v| remap[v as usize]))
            .collect();
        let mesh = Mesh::new(positions, triangles).map_err(|e| OpError::Failed {
            code: "invalid_output",
            message: e.to_string(),
        })?;
        Ok(OpOutput {
            mesh: Some(mesh),
            report: json!({
                "removed_components": removed_components,
                "removed_triangles": removed_triangles,
            }),
        })
    }
}
