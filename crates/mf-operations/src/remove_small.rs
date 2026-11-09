use mf_geometry::{FaceId, Mesh, VertexId};
use mf_spec::Params;
use serde_json::json;

use crate::{OpError, OpOutput, Operation, RunContext, Target, unsupported_target};

/// `mesh.remove_small_components`. On a whole-object run a piece is
/// "small" when it has fewer than `max_triangles` triangles *and* no more
/// than `max_relative_size` percent of the largest piece's (MF-ISSUE-052:
/// with only the absolute count, the defaults deleted 1,050 of the Verlan
/// dress's 1,100 parts and 2,207 of the OpenRC soup's 2,208 pieces).
/// Version 1 had no relative limit; its records replay with it at 100%.
pub struct RemoveSmallComponents;

/// What a whole-object run would remove, worked out from the component
/// sizes alone (so a panel can show it before Apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovalPreview {
    pub pieces: u64,
    pub of_pieces: u64,
    pub triangles: u64,
    pub of_triangles: u64,
}

impl RemovalPreview {
    /// A removal of more than half of the pieces or more than 10% of the
    /// triangles may be taking parts of the design, not debris: the user
    /// confirms it first.
    pub fn needs_confirmation(&self) -> bool {
        2 * self.pieces > self.of_pieces || 10 * self.triangles > self.of_triangles
    }
}

/// The whole-object candidates' rule: component `i` (of `size` triangles;
/// components ordered largest first, so `largest` is component 0's size).
fn is_candidate(
    i: usize,
    size: usize,
    largest: usize,
    max_triangles: usize,
    max_relative: f64,
    keep_largest: bool,
) -> bool {
    !(keep_largest && i == 0)
        && size < max_triangles
        && size as f64 * 100.0 <= max_relative * largest as f64
}

struct Thresholds {
    max_triangles: usize,
    max_relative: f64,
    keep_largest: bool,
}

fn thresholds(params: &Params) -> Result<Thresholds, OpError> {
    Ok(Thresholds {
        max_triangles: params.get_int("max_triangles")?.max(0) as usize,
        max_relative: params.get_float("max_relative_size")?,
        keep_largest: params.get_bool("keep_largest")?,
    })
}

/// What a whole-object run with `params` removes from a mesh whose
/// components have `sizes` triangles (largest first, as the topology
/// orders them). O(components log components); see [`ComponentSizes`]
/// to ask again and again.
pub fn removal_preview(sizes: &[usize], params: &Params) -> Result<RemovalPreview, OpError> {
    let t = thresholds(params)?;
    Ok(ComponentSizes::new(sizes).preview(t.max_triangles, t.max_relative, t.keep_largest))
}

/// A mesh's component sizes, indexed so that what a whole-object run would
/// remove can be asked for any thresholds in O(log components): a panel
/// asks every frame while its sliders move.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComponentSizes {
    /// The largest component's triangles (component 0's).
    largest: usize,
    /// Every other component's, ascending…
    rest: Vec<usize>,
    /// …and their running totals: `prefix[k]` is the sum of `rest[..k]`.
    prefix: Vec<u64>,
}

impl ComponentSizes {
    /// From component sizes, largest first (the topology's order).
    /// O(n log n).
    pub fn new(sizes: &[usize]) -> ComponentSizes {
        // allow-swallow: a mesh with no components has no largest one to compare with
        let largest = sizes.first().copied().unwrap_or(0);
        let mut rest = sizes.get(1..).map_or_else(Vec::new, <[usize]>::to_vec);
        rest.sort_unstable();
        let mut prefix = Vec::with_capacity(rest.len() + 1);
        prefix.push(0u64);
        for &s in &rest {
            prefix.push(prefix[prefix.len() - 1] + s as u64);
        }
        ComponentSizes {
            largest,
            rest,
            prefix,
        }
    }

    /// Components, the largest included.
    pub fn count(&self) -> usize {
        self.rest.len() + usize::from(self.largest > 0 || !self.rest.is_empty())
    }

    /// What a whole-object run with these thresholds removes (the same rule
    /// as the run). O(log n).
    pub fn preview(
        &self,
        max_triangles: usize,
        max_relative: f64,
        keep_largest: bool,
    ) -> RemovalPreview {
        let candidate = |i: usize, s: usize| {
            is_candidate(
                i,
                s,
                self.largest,
                max_triangles,
                max_relative,
                keep_largest,
            )
        };
        // The rule is monotone in size: the candidates are a prefix.
        let k = self.rest.partition_point(|&s| candidate(1, s));
        let total = self.prefix[self.rest.len()] + self.largest as u64;
        let mut out = RemovalPreview {
            pieces: k as u64,
            of_pieces: self.count() as u64,
            triangles: self.prefix[k],
            of_triangles: total,
        };
        if self.count() > 0 && candidate(0, self.largest) {
            out.pieces += 1;
            out.triangles += self.largest as u64;
        }
        out
    }

    /// The largest size a candidate can have with these thresholds (every
    /// smaller piece is one too; `None`: no piece but perhaps the largest).
    pub fn candidate_limit(&self, max_triangles: usize, max_relative: f64) -> Option<usize> {
        let k = self.rest.partition_point(|&s| {
            is_candidate(1, s, self.largest, max_triangles, max_relative, false)
        });
        k.checked_sub(1).map(|i| self.rest[i])
    }
}

impl Operation for RemoveSmallComponents {
    fn id(&self) -> &'static str {
        "mesh.remove_small_components"
    }

    fn version(&self) -> u32 {
        2
    }

    fn upgrade_params(&self, recorded: u32, params: &mut Params) -> bool {
        match recorded {
            // Version 1 had no relative limit: 100% keeps none back.
            1 => {
                params.0.insert(
                    "max_relative_size".to_owned(),
                    mf_spec::ParamValue::Float(100.0),
                );
                true
            }
            v => v == self.version(),
        }
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        params: &Params,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        let t = thresholds(params)?;
        let max_triangles = t.max_triangles;
        let topo = input.topology_with(ctx)?;

        // Which components are candidates: all of them, or the selected ones.
        // A selected component is removed only if it is under the threshold;
        // otherwise that's an error naming it, never a silent no-op.
        // `keep_largest` and the relative limit apply to whole-object runs
        // only: an explicit selection already says which component to remove.
        let candidates: Vec<usize> = match target {
            Target::WholeObject => {
                let largest = topo.components.first().map_or(0, Vec::len);
                (0..topo.components.len())
                    .filter(|&i| {
                        is_candidate(
                            i,
                            topo.components[i].len(),
                            largest,
                            max_triangles,
                            t.max_relative,
                            t.keep_largest,
                        )
                    })
                    .collect()
            }
            Target::Components(ids) => {
                let mut picked = Vec::new();
                for &c in ids {
                    let comp = topo
                        .components
                        .get(c as usize)
                        .ok_or_else(|| OpError::Failed {
                            code: "invalid_target",
                            message: format!("component {c} does not exist"),
                        })?;
                    if comp.len() >= max_triangles {
                        return Err(OpError::Failed {
                            code: "component_too_large",
                            message: format!(
                                "component {c} has {} triangles, not fewer than max_triangles {max_triangles}",
                                comp.len()
                            ),
                        });
                    }
                    picked.push(c as usize);
                }
                picked.sort_unstable();
                picked.dedup();
                picked
            }
            other => return Err(unsupported_target(self.id(), other)),
        };

        const STAGE: &str = "removing";
        let mut tick = ctx.ticker();
        let mut keep_face = vec![true; input.face_slots() as usize];
        let mut removed_components = 0;
        let mut removed_triangles = 0;
        for &i in &candidates {
            for &f in &topo.components[i] {
                tick.tick()?;
                keep_face[f as usize] = false;
            }
            removed_components += 1;
            removed_triangles += topo.components[i].len();
        }

        let report = json!({
            "removed_components": removed_components,
            "removed_triangles": removed_triangles,
        });
        if removed_components == 0 {
            ctx.progress.report(STAGE, 1.0);
            return Ok(OpOutput {
                change: None,
                report,
            });
        }

        // Tombstone removed faces, and vertices used only by removed faces.
        // Kept faces and vertices keep their IDs (ADR-0001, MF-OP-003).
        let mut used_by_kept = vec![false; input.vertex_slots() as usize];
        let mut used_by_removed = vec![false; input.vertex_slots() as usize];
        let span = ctx.span(STAGE, 0.0, 0.5, input.face_count());
        for (k, (FaceId(f), t)) in input.faces().enumerate() {
            span.at(k)?;
            let used = if keep_face[f as usize] {
                &mut used_by_kept
            } else {
                &mut used_by_removed
            };
            for v in t {
                used[v as usize] = true;
            }
        }
        let mut edit = input.edit();
        let fail = |e: mf_geometry::GeometryError| OpError::Failed {
            code: "invalid_edit",
            message: e.to_string(),
        };
        let span = ctx.span(STAGE, 0.5, 0.9, input.face_count());
        for (k, (FaceId(f), _)) in input.faces().enumerate() {
            span.at(k)?;
            if !keep_face[f as usize] {
                edit.remove_face(FaceId(f)).map_err(fail)?;
            }
        }
        for v in 0..input.vertex_slots() {
            tick.tick()?;
            if used_by_removed[v as usize] && !used_by_kept[v as usize] {
                edit.remove_vertex(VertexId(v)).map_err(fail)?;
            }
        }
        ctx.checkpoint()?;
        let change = edit.finish().map_err(fail)?;
        ctx.progress.report(STAGE, 1.0);
        Ok(OpOutput {
            change: Some(change),
            report,
        })
    }
}
