//! STL reader/writer. Binary is detected by exact size (`84 + 50·n`), not by
//! the `solid` prefix, because many binary exporters write `solid` headers.

use mf_geometry::Mesh;

use crate::{IoError, SourcePrecision, significant_digits};

const HEADER: usize = 80;
const FACET: usize = 50;

pub fn read(bytes: &[u8]) -> Result<(Mesh, SourcePrecision), IoError> {
    if bytes.len() >= HEADER + 4 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        let expected = HEADER + 4 + FACET * n;
        if expected == bytes.len() {
            return read_binary(&bytes[HEADER + 4..], n).map(|m| (m, SourcePrecision::F32));
        }
        if !looks_ascii(bytes) {
            return Err(IoError::Truncated {
                expected,
                actual: bytes.len(),
            });
        }
    } else if !looks_ascii(bytes) {
        return Err(IoError::Truncated {
            expected: HEADER + 4,
            actual: bytes.len(),
        });
    }
    read_ascii(bytes)
}

fn looks_ascii(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    bytes[start..].starts_with(b"solid")
}

fn read_binary(body: &[u8], n: usize) -> Result<Mesh, IoError> {
    let mut corners: Vec<[f32; 3]> = Vec::with_capacity(n * 3);
    for facet in body.as_chunks::<FACET>().0 {
        for v in 0..3 {
            let off = 12 + v * 12;
            let f = |k: usize| {
                let o = off + k * 4;
                f32::from_le_bytes([facet[o], facet[o + 1], facet[o + 2], facet[o + 3]])
            };
            corners.push([f(0), f(1), f(2)]);
        }
    }
    let corners: Vec<[f64; 3]> = corners.iter().map(|c| c.map(f64::from)).collect();
    weld(&corners)
}

fn read_ascii(bytes: &[u8]) -> Result<(Mesh, SourcePrecision), IoError> {
    let text = std::str::from_utf8(bytes).map_err(|e| IoError::Parse {
        line: 0,
        message: format!("ASCII STL is not valid UTF-8: {e}"),
    })?;
    let mut corners = Vec::new();
    let mut digits = 0u8;
    for (i, line) in text.lines().enumerate() {
        let mut tok = line.split_ascii_whitespace();
        let Some(kw) = tok.next() else { continue };
        let want_floats = match kw {
            "vertex" => true,
            "facet" => {
                if tok.next() != Some("normal") {
                    return Err(parse(i, "expected 'facet normal'"));
                }
                false
            }
            _ => continue,
        };
        let mut v = [0.0f64; 3];
        for c in &mut v {
            let t = tok
                .next()
                .ok_or_else(|| parse(i, "expected 3 coordinates"))?;
            *c = t
                .parse()
                .map_err(|_| parse(i, &format!("invalid number '{t}'")))?;
            if want_floats {
                digits = digits.max(significant_digits(t));
            }
        }
        if want_floats {
            corners.push(v);
        }
    }
    if corners.len() % 3 != 0 {
        return Err(parse(
            text.lines().count(),
            "vertex count is not a multiple of 3",
        ));
    }
    Ok((
        weld(&corners)?,
        SourcePrecision::Decimal {
            max_significant_digits: digits,
        },
    ))
}

fn parse(line0: usize, message: &str) -> IoError {
    IoError::Parse {
        line: line0 + 1,
        message: message.to_owned(),
    }
}

/// Merge exactly-equal corner positions, in first-occurrence order.
/// Sort-based, so the result is deterministic.
fn weld(corners: &[[f64; 3]]) -> Result<Mesh, IoError> {
    // `+ 0.0` folds -0.0 into 0.0 so they weld together.
    let key = |p: &[f64; 3]| p.map(|c| (c + 0.0).to_bits());
    let mut order: Vec<u32> = (0..corners.len() as u32).collect();
    order.sort_unstable_by_key(|&i| (key(&corners[i as usize]), i));
    // Group leaders (min corner index) in sorted order.
    let mut leader = vec![0u32; corners.len()];
    let mut groups: Vec<u32> = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        if k == 0 || key(&corners[order[k - 1] as usize]) != key(&corners[i as usize]) {
            groups.push(i);
        }
        leader[i as usize] = *groups.last().unwrap_or(&i);
    }
    groups.sort_unstable();
    let mut id_of_leader = vec![u32::MAX; corners.len()];
    let mut positions = Vec::with_capacity(groups.len());
    for (id, &g) in groups.iter().enumerate() {
        id_of_leader[g as usize] = id as u32;
        positions.push(corners[g as usize].map(|c| c + 0.0));
    }
    let triangles = (0..corners.len() / 3)
        .map(|f| [0, 1, 2].map(|k| id_of_leader[leader[f * 3 + k] as usize]))
        .collect();
    Ok(Mesh::new(positions, triangles)?)
}

fn facet_normal(m: &Mesh, t: [u32; 3]) -> [f32; 3] {
    let p = |i: u32| m.positions()[i as usize];
    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 0.0 {
        n.map(|x| (x / len) as f32)
    } else {
        [0.0; 3]
    }
}

/// Binary STL stores `f32`. Coordinates that don't fit are an error, never `inf`.
pub fn write_binary(mesh: &Mesh) -> Result<Vec<u8>, String> {
    if let Some(v) = mesh
        .positions()
        .iter()
        .position(|p| p.iter().any(|&c| !(c as f32).is_finite()))
    {
        return Err(format!(
            "vertex {v} has a coordinate outside the 32-bit float range binary STL can store"
        ));
    }
    let mut out = Vec::with_capacity(HEADER + 4 + FACET * mesh.triangle_count());
    let mut header = [b' '; HEADER];
    let tag = b"MeshFixer binary STL";
    header[..tag.len()].copy_from_slice(tag);
    out.extend_from_slice(&header);
    out.extend_from_slice(&(mesh.triangle_count() as u32).to_le_bytes());
    for &t in mesh.triangles() {
        for c in facet_normal(mesh, t) {
            out.extend_from_slice(&c.to_le_bytes());
        }
        for i in t {
            for c in mesh.positions()[i as usize] {
                out.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    Ok(out)
}

pub fn write_ascii(mesh: &Mesh) -> String {
    use std::fmt::Write;
    let mut s = String::from("solid meshfixer\n");
    for &t in mesh.triangles() {
        let n = facet_normal(mesh, t);
        let _ = writeln!(
            s,
            "  facet normal {} {} {}\n    outer loop",
            n[0], n[1], n[2]
        );
        for i in t {
            let p = mesh.positions()[i as usize];
            let _ = writeln!(s, "      vertex {} {} {}", p[0], p[1], p[2]);
        }
        s.push_str("    endloop\n  endfacet\n");
    }
    s.push_str("endsolid meshfixer\n");
    s
}
