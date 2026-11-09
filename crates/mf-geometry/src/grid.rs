//! Compact uniform grid for "points within distance t" queries
//! (MF-ISSUE-026, MF-ISSUE-028).
//!
//! Cells are keyed relative to the points' bounding-box minimum, so large
//! coordinate offsets can't saturate the integer keys, and the cell size is
//! clamped so keys stay well inside `i64`. Entries live in one sorted array
//! (about 32 bytes per point) instead of a hash map of vectors.

use mf_runtime::{Cancelled, RunContext, par_sort_unstable_by_key};

pub struct PointGrid {
    origin: [f64; 3],
    cell: f64,
    /// (cell key, point index), sorted by key then index.
    entries: Vec<([i64; 3], u32)>,
}

impl PointGrid {
    /// Build a grid whose cells are at least `min_cell` wide. Any two points
    /// closer than `min_cell` are in the same or adjacent cells.
    /// `points` is called twice (bounds, then keys).
    pub fn new<I: Iterator<Item = (u32, [f64; 3])>>(
        points: impl Fn() -> I,
        min_cell: f64,
    ) -> PointGrid {
        PointGrid::new_with(points, min_cell, &RunContext::none())
            .expect("a context that is never cancelled can't be cancelled")
    }

    /// As [`PointGrid::new`], cancellable.
    pub fn new_with<I: Iterator<Item = (u32, [f64; 3])>>(
        points: impl Fn() -> I,
        min_cell: f64,
        ctx: &RunContext,
    ) -> Result<PointGrid, Cancelled> {
        let mut tick = ctx.ticker();
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for (_, p) in points() {
            tick.tick()?;
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let extent = (0..3).map(|k| hi[k] - lo[k]).fold(0.0f64, f64::max);
        // Keep keys within ±2^40 per axis.
        let cell = min_cell
            .max(extent / (1u64 << 40) as f64)
            .max(f64::MIN_POSITIVE);
        let origin = if lo[0].is_finite() { lo } else { [0.0; 3] };
        let mut grid = PointGrid {
            origin,
            cell,
            entries: Vec::new(),
        };
        let mut entries = Vec::new();
        for (i, p) in points() {
            tick.tick()?;
            entries.push((grid.key(p), i));
        }
        par_sort_unstable_by_key(&mut entries, ctx, |e| *e)?;
        grid.entries = entries;
        Ok(grid)
    }

    fn key(&self, p: [f64; 3]) -> [i64; 3] {
        [0, 1, 2].map(|k| ((p[k] - self.origin[k]) / self.cell).floor() as i64)
    }

    /// Indices of points in the 3×3×3 cells around `p`, in ascending key
    /// order (deterministic).
    pub fn near(&self, p: [f64; 3]) -> impl Iterator<Item = u32> + '_ {
        let c = self.key(p);
        (-1i64..=1).flat_map(move |dx| {
            (-1i64..=1).flat_map(move |dy| {
                (-1i64..=1).flat_map(move |dz| {
                    let k = [
                        c[0].saturating_add(dx),
                        c[1].saturating_add(dy),
                        c[2].saturating_add(dz),
                    ];
                    let start = self.entries.partition_point(|(e, _)| *e < k);
                    self.entries[start..]
                        .iter()
                        .take_while(move |(e, _)| *e == k)
                        .map(|(_, i)| *i)
                })
            })
        })
    }
}
