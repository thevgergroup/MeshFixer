//! Wavefront OBJ geometry (`v` and `f` only). Other directives are skipped and
//! reported as one aggregated warning per keyword.

use mf_geometry::Mesh;

use crate::{IoError, SourcePrecision, significant_digits};

pub fn read(text: &str) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError> {
    let mut positions: Vec<[f64; 3]> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut skipped: Vec<(String, usize)> = Vec::new();
    let mut digits = 0u8;
    let mut poly: Vec<u32> = Vec::new();

    for (i, line) in text.lines().enumerate() {
        let line_no = i + 1;
        let err = |message: String| IoError::Parse {
            line: line_no,
            message,
        };
        let line = line.split('#').next().unwrap_or("");
        let mut tok = line.split_ascii_whitespace();
        let Some(kw) = tok.next() else { continue };
        match kw {
            "v" => {
                let mut p = [0.0; 3];
                for c in &mut p {
                    let t = tok
                        .next()
                        .ok_or_else(|| err("vertex needs 3 coordinates".into()))?;
                    *c = t
                        .parse()
                        .map_err(|_| err(format!("invalid number '{t}'")))?;
                    digits = digits.max(significant_digits(t));
                }
                positions.push(p);
            }
            "f" => {
                poly.clear();
                for t in tok {
                    let idx = t.split('/').next().unwrap_or("");
                    let raw: i64 = idx
                        .parse()
                        .map_err(|_| err(format!("invalid face index '{t}'")))?;
                    let n = positions.len() as i64;
                    let resolved = match raw {
                        r if r > 0 => r - 1,
                        r if r < 0 => n + r,
                        _ => return Err(err("face index 0 is invalid".into())),
                    };
                    if resolved < 0 || resolved >= n {
                        return Err(err(format!(
                            "face index {raw} is out of range ({n} vertices so far)"
                        )));
                    }
                    poly.push(resolved as u32);
                }
                if poly.len() < 3 {
                    return Err(err("face needs at least 3 vertices".into()));
                }
                for k in 1..poly.len() - 1 {
                    triangles.push([poly[0], poly[k], poly[k + 1]]);
                }
            }
            other => match skipped.iter_mut().find(|(k, _)| k == other) {
                Some((_, count)) => *count += 1,
                None => skipped.push((other.to_owned(), 1)),
            },
        }
    }
    let warnings = skipped
        .into_iter()
        .map(|(k, n)| format!("skipped {n} '{k}' line(s): only v/f are imported"))
        .collect();
    Ok((
        Mesh::new(positions, triangles)?,
        SourcePrecision::Decimal {
            max_significant_digits: digits,
        },
        warnings,
    ))
}

/// Writes positions with shortest round-trip `f64` formatting, so no precision is lost.
pub fn write(mesh: &Mesh) -> String {
    use std::fmt::Write;
    let mut s = String::from("# MeshFixer OBJ\n");
    for p in mesh.positions() {
        let _ = writeln!(s, "v {} {} {}", p[0], p[1], p[2]);
    }
    for t in mesh.triangles() {
        let _ = writeln!(s, "f {} {} {}", t[0] + 1, t[1] + 1, t[2] + 1);
    }
    s
}
