//! `layout_on_plate` (D21): one shared, deterministic way to lay new loose
//! parts out on the build plate, avoiding everything already there.
//!
//! Rows are packed from the plate's corner nearest the origin (−x, −y),
//! left to right, then the next row further in +y. A candidate spot is
//! taken when its rectangle (grown by `spacing`) overlaps no occupied
//! footprint and no part already placed, and lies on the plate. Order is
//! fixed by input order, so replay is Exact. Parts that don't fit are
//! placed in a row beside the plate (+x) and listed in `off_plate`.

/// A world XY bounding rectangle: `[min_x, min_y, max_x, max_y]`.
pub type Footprint = [f64; 4];

/// The plate: centred on the origin, `size` wide (x) and deep (y), in
/// document units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plate {
    pub size: [f64; 2],
}

/// Where each new part goes: a translation-only transform per part (it
/// moves the part's footprint's min corner to the chosen spot), and the
/// parts that didn't fit on the plate.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutResult {
    pub transforms: Vec<[f64; 12]>,
    pub off_plate: Vec<usize>,
}

fn overlaps(a: &Footprint, b: &Footprint) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn translate(dx: f64, dy: f64) -> [f64; 12] {
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, dx, dy, 0.0]
}

/// Lays `new` out (see the module docs). `new` footprints are where the
/// parts are now; the transforms are translations to apply on top.
pub fn layout_on_plate(
    occupied: &[Footprint],
    new: &[Footprint],
    plate: &Plate,
    spacing: f64,
) -> LayoutResult {
    let (hx, hy) = (plate.size[0] / 2.0, plate.size[1] / 2.0);
    let mut taken: Vec<Footprint> = occupied
        .iter()
        .map(|f| {
            [
                f[0] - spacing,
                f[1] - spacing,
                f[2] + spacing,
                f[3] + spacing,
            ]
        })
        .collect();
    let step = (spacing.max(plate.size[0].min(plate.size[1]) / 256.0)).max(f64::MIN_POSITIVE);
    let mut transforms = Vec::with_capacity(new.len());
    let mut off_plate = Vec::new();
    let mut beside_x = hx + spacing;
    for (i, f) in new.iter().enumerate() {
        let (w, d) = (f[2] - f[0], f[3] - f[1]);
        let mut spot = None;
        let mut y = -hy;
        'rows: while y + d <= hy {
            let mut x = -hx;
            while x + w <= hx {
                let cand = [x, y, x + w, y + d];
                match taken.iter().find(|t| overlaps(&cand, t)) {
                    // Jump past the obstacle.
                    Some(t) => x = (t[2]).max(x + step),
                    None => {
                        spot = Some((x, y));
                        break 'rows;
                    }
                }
            }
            y += step;
        }
        let (x, y) = match spot {
            Some(s) => s,
            None => {
                off_plate.push(i);
                let s = (beside_x, -hy);
                beside_x += w + spacing;
                s
            }
        };
        taken.push([x - spacing, y - spacing, x + w + spacing, y + d + spacing]);
        transforms.push(translate(x - f[0], y - f[1]));
    }
    LayoutResult {
        transforms,
        off_plate,
    }
}
