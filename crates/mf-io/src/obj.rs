//! Wavefront OBJ geometry (`v` and `f` only). Other directives are skipped and
//! reported as one aggregated warning per keyword.

use mf_geometry::Mesh;

use mf_runtime::{RunContext, TICK_INTERVAL};

use crate::{IoError, READING, SourcePrecision, parsed, significant_digits};

pub fn read(text: &str) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError> {
    read_with(text, &RunContext::none())
}

/// As [`read`], cancellable, reporting stage `"reading"` by bytes parsed.
pub fn read_with(
    text: &str,
    ctx: &RunContext,
) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError> {
    let mut positions: Vec<[f64; 3]> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut skipped: Vec<(String, usize)> = Vec::new();
    let mut digits = 0u8;
    let mut poly: Vec<u32> = Vec::new();

    for (i, line) in text.lines().enumerate() {
        if i.is_multiple_of(TICK_INTERVAL as usize) {
            parsed(ctx, text, line)?;
        }
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
    let mesh = Mesh::new_with(positions, triangles, ctx)??;
    ctx.progress.report(READING, 1.0);
    Ok((
        mesh,
        SourcePrecision::Decimal {
            max_significant_digits: digits,
        },
        warnings,
    ))
}

/// Writes live vertices and faces with shortest round-trip `f64` formatting,
/// so no precision is lost. Vertex slots are renumbered densely.
pub fn write(mesh: &Mesh) -> String {
    use std::fmt::Write;
    // Pre-size so the buffer never doubles (doubling briefly holds two copies).
    let estimate = 16 + mesh.vertex_count() * 64 + mesh.face_count() * 32;
    let mut s = String::with_capacity(estimate);
    s.push_str("# MeshFixer OBJ\n");
    // 1-based OBJ index per vertex slot (0 for tombstoned slots).
    let mut index = vec![0u32; mesh.vertex_slots() as usize];
    for (n, (id, p)) in mesh.vertices().enumerate() {
        index[id.0 as usize] = n as u32 + 1;
        // allow-swallow: writing to a String can't fail
        let _ = writeln!(s, "v {} {} {}", p[0], p[1], p[2]);
    }
    for (_, t) in mesh.faces() {
        let [a, b, c] = t.map(|v| index[v as usize]);
        // allow-swallow: writing to a String can't fail
        let _ = writeln!(s, "f {a} {b} {c}");
    }
    s
}

/// Check written OBJ text against the mesh without building a new mesh:
/// every `v` line must match the next live vertex exactly, and every `f`
/// line the next live face (renumbered as [`write`] does).
pub(crate) fn verify(text: &str, mesh: &Mesh) -> Result<(), String> {
    let mut index = vec![0u32; mesh.vertex_slots() as usize];
    for (n, (id, _)) in mesh.vertices().enumerate() {
        index[id.0 as usize] = n as u32 + 1;
    }
    let mut vertices = mesh.vertices();
    let mut faces = mesh.faces();
    for (i, line) in text.lines().enumerate() {
        let mut tok = line.split_ascii_whitespace();
        match tok.next() {
            Some("v") => {
                let expected = vertices
                    .next()
                    .ok_or_else(|| format!("line {}: extra vertex", i + 1))?
                    .1;
                for c in expected {
                    let got: f64 = tok
                        .next()
                        // allow-swallow: a parse failure becomes the positioned error on the next line
                        .and_then(|t| t.parse().ok())
                        .ok_or_else(|| format!("line {}: bad vertex", i + 1))?;
                    if got.to_bits() != c.to_bits() {
                        return Err(format!("line {}: vertex {got} != {c}", i + 1));
                    }
                }
            }
            Some("f") => {
                let expected = faces
                    .next()
                    .ok_or_else(|| format!("line {}: extra face", i + 1))?
                    .1;
                for v in expected {
                    let got: u32 = tok
                        .next()
                        // allow-swallow: a parse failure becomes the positioned error on the next line
                        .and_then(|t| t.parse().ok())
                        .ok_or_else(|| format!("line {}: bad face", i + 1))?;
                    // 0 is never a valid OBJ index (1-based), even if a
                    // tombstoned slot would map to it.
                    if got == 0 || got != index[v as usize] {
                        return Err(format!(
                            "line {}: face index {got} != {}",
                            i + 1,
                            index[v as usize]
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if vertices.next().is_some() || faces.next().is_some() {
        return Err("output is missing vertices or faces".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri() -> Mesh {
        Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
        .expect("valid")
    }

    #[test]
    fn verify_accepts_own_output() {
        assert_eq!(verify(&write(&tri()), &tri()), Ok(()));
    }

    #[test]
    fn verify_catches_changed_coordinate_index_and_truncation() {
        let text = write(&tri());
        assert!(verify(&text.replace("v 1 0 0", "v 1.0000001 0 0"), &tri()).is_err());
        assert!(verify(&text.replace("f 1 2 3", "f 1 3 2"), &tri()).is_err());
        let truncated: String = text.lines().take(3).map(|l| format!("{l}\n")).collect();
        assert!(verify(&truncated, &tri()).is_err());
    }
}

#[cfg(test)]
mod verify_index_zero {
    use super::*;

    #[test]
    fn verify_rejects_index_zero() {
        let m = Mesh::new(
            vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
        .expect("valid");
        let bad = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 0\n";
        assert!(verify(bad, &m).is_err());
    }
}
