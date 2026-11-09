//! STL reader/writer. Binary is detected by exact size (`84 + 50·n`), not by
//! the `solid` prefix, because many binary exporters write `solid` headers.

use mf_geometry::{Mesh, VertexId};
use mf_runtime::{RunContext, TICK_INTERVAL, indices, par_sort_unstable_by_key};

use crate::{IoError, Limits, READING, SourcePrecision, WELDING, parsed, significant_digits};

const HEADER: usize = 80;
const FACET: usize = 50;

/// Read binary or ASCII STL. Returns the mesh, the source precision and any
/// warnings (MF-ISSUE-002):
/// - a binary header facet count of 0 (a common exporter placeholder) with a
///   size that holds whole facets is read by size, with a warning;
/// - bytes after the declared facets are ignored, with a warning;
/// - a file shorter than its header declares is an error (truncation and a
///   wrong count can't be told apart, so nothing is guessed);
/// - a `solid` header on a file that is neither valid ASCII STL nor a
///   size-consistent binary STL is reported as that ambiguity.
pub fn read(bytes: &[u8]) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError> {
    read_with(bytes, &RunContext::none())
}

/// As [`read`], cancellable. ASCII text is parsed as stage `"reading"`;
/// both encodings then weld as stage `"welding"`.
pub fn read_with(
    bytes: &[u8],
    ctx: &RunContext,
) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError> {
    read_with_limits(bytes, ctx, Limits::default())
}

/// As [`read_with`], with explicit [`Limits`] (tests use tighter ones than
/// the defaults so hostile-input tests don't need gigabyte fixtures).
pub fn read_with_limits(
    bytes: &[u8],
    ctx: &RunContext,
    limits: Limits,
) -> Result<(Mesh, SourcePrecision, Vec<String>), IoError> {
    let ascii = looks_ascii(bytes);
    let len = bytes.len();
    let mut header = None;
    if len >= HEADER + 4 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as u64;
        // A genuinely ASCII file's bytes 80..84 are just text and may decode
        // to any 32-bit value; only a file that is actually going to be read
        // as binary (size-consistent, or not ASCII at all) commits to `n`,
        // and that commitment is checked against the limit before anything
        // downstream sizes an allocation to it (MF-IO-002): a hostile header
        // declaring billions of facets must never reach
        // `Vec::with_capacity(n)`.
        let expected_if_binary = (HEADER + 4) as u64 + FACET as u64 * n;
        let commits_to_binary = expected_if_binary == len as u64 || !ascii;
        if commits_to_binary && n > limits.max_triangles {
            return Err(IoError::LimitExceeded {
                what: "declared STL facet count",
                limit: limits.max_triangles,
                found: n,
            });
        }
        // u64: `50 * n` can't overflow even on 32-bit targets (MF-ISSUE-001).
        let expected = expected_if_binary;
        header = Some((n, expected));
        let body = &bytes[HEADER + 4..];
        let whole = body.len() / FACET;
        if expected == len as u64 {
            return read_binary(body, n as usize, ctx)
                .map(|m| (m, SourcePrecision::F32, Vec::new()));
        }
        if !ascii {
            if n == 0 && body.len().is_multiple_of(FACET) {
                let warning = format!(
                    "binary STL header facet count is 0; read {whole} facets from the file size"
                );
                return read_binary(&body[..whole * FACET], whole, ctx)
                    .map(|m| (m, SourcePrecision::F32, vec![warning]));
            }
            if expected < len as u64 {
                let extra = len as u64 - expected;
                let warning = format!("ignored {extra} trailing bytes after {n} facets");
                let end = (expected as usize) - (HEADER + 4);
                return read_binary(&body[..end], n as usize, ctx)
                    .map(|m| (m, SourcePrecision::F32, vec![warning]));
            }
            return Err(IoError::Truncated {
                expected: expected as usize,
                actual: len,
                detail: format!(
                    " (header declares {n} facets, the file holds {whole} complete facets: \
                     it is truncated, or the header count is wrong)"
                ),
            });
        }
    } else if !ascii {
        return Err(IoError::Truncated {
            expected: HEADER + 4,
            actual: len,
            detail: " (too short for a binary STL header)".into(),
        });
    }
    if std::str::from_utf8(bytes).is_err()
        && let Some((n, expected)) = header
    {
        return Err(IoError::Parse {
            line: 0,
            message: format!(
                "binary STL with an ASCII-looking 'solid' header: its size ({len} bytes) \
                 doesn't match its {n} declared facets ({expected} bytes), and it isn't text"
            ),
        });
    }
    read_ascii(bytes, ctx).map(|(m, p)| (m, p, Vec::new()))
}

fn looks_ascii(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    bytes[start..].starts_with(b"solid")
}

/// Corner `c`'s coordinates, read straight from the facet bytes.
fn corner(body: &[u8], c: usize) -> [f32; 3] {
    let off = (c / 3) * FACET + 12 + (c % 3) * 12;
    let f = |k: usize| {
        let o = off + k * 4;
        f32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]])
    };
    // `+ 0.0` folds -0.0 into 0.0 so they weld together.
    [f(0) + 0.0, f(1) + 0.0, f(2) + 0.0]
}

/// Weld binary STL corners on exactly equal positions, vertices in
/// first-occurrence order. Works from the file bytes without copying the
/// corners: peak scratch is ~24 bytes per triangle plus ~12 per vertex
/// (ADR-0001 measurements).
fn read_binary(body: &[u8], n: usize, ctx: &RunContext) -> Result<Mesh, IoError> {
    // The file is read; nothing is left to parse.
    ctx.progress.report(READING, 1.0);
    let corners = n * 3;
    let key = |c: u32| corner(body, c as usize).map(f32::to_bits);
    ctx.progress.report(WELDING, 0.0);
    let mut order = indices(corners as u32, ctx)?;
    // Parallel, and still deterministic: (key, corner) is a total order.
    par_sort_unstable_by_key(&mut order, ctx, |&c| (key(c), c))?;

    // Group sorted corners; each group's first corner is its leader
    // (smallest corner index, since the sort key includes it).
    // Zeroed in blocks: tens of megabytes, freshly touched.
    let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(n);
    while triangles.len() < n {
        ctx.checkpoint()?;
        let len = (triangles.len() + TICK_INTERVAL as usize).min(n);
        triangles.resize(len, [0; 3]);
    }
    let span = ctx.span(WELDING, 0.6, 0.8, order.len());
    let mut leaders: Vec<u32> = Vec::new();
    let mut prev = None;
    for (i, &c) in order.iter().enumerate() {
        span.at(i)?;
        let k = key(c);
        if prev != Some(k) {
            leaders.push(c);
            prev = Some(k);
        }
        triangles[c as usize / 3][c as usize % 3] = leaders.len() as u32 - 1;
    }
    drop(order);

    // Vertex IDs in first-occurrence order = groups sorted by leader.
    let mut by_leader = indices(leaders.len() as u32, ctx)?;
    // Leaders are distinct corners: a total order.
    par_sort_unstable_by_key(&mut by_leader, ctx, |&g| leaders[g as usize])?;
    let span = ctx.span(WELDING, 0.85, 0.9, by_leader.len());
    let mut vertex_of_group = vec![0u32; leaders.len()];
    let mut positions = Vec::with_capacity(leaders.len());
    for (v, &g) in by_leader.iter().enumerate() {
        span.at(v)?;
        vertex_of_group[g as usize] = v as u32;
        positions.push(corner(body, leaders[g as usize] as usize).map(f64::from));
    }
    drop(by_leader);
    drop(leaders);
    let span = ctx.span(WELDING, 0.9, 0.95, triangles.len());
    for (i, t) in triangles.iter_mut().enumerate() {
        span.at(i)?;
        *t = t.map(|g| vertex_of_group[g as usize]);
    }
    finish(positions, triangles, ctx)
}

/// Build the welded mesh: the last step of `"welding"`.
fn finish(
    positions: Vec<[f64; 3]>,
    triangles: Vec<[u32; 3]>,
    ctx: &RunContext,
) -> Result<Mesh, IoError> {
    let mesh = Mesh::new_with(positions, triangles, ctx)??;
    ctx.progress.report(WELDING, 1.0);
    Ok(mesh)
}

/// Structural check of encoded binary STL: size, facet count and finite
/// coordinates. Cheaper than a full re-import, and enough because the bytes
/// came from a valid mesh.
pub(crate) fn verify_binary(bytes: &[u8], triangles: usize) -> Result<(), String> {
    let expected = HEADER + 4 + FACET * triangles;
    if bytes.len() != expected {
        return Err(format!(
            "encoded {} bytes, expected {expected}",
            bytes.len()
        ));
    }
    let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    if n != triangles {
        return Err(format!("header says {n} facets, expected {triangles}"));
    }
    let body = &bytes[HEADER + 4..];
    if (0..n * 3).any(|c| corner(body, c).iter().any(|x| !x.is_finite())) {
        return Err("encoded a non-finite coordinate".into());
    }
    Ok(())
}

fn read_ascii(bytes: &[u8], ctx: &RunContext) -> Result<(Mesh, SourcePrecision), IoError> {
    let text = std::str::from_utf8(bytes).map_err(|e| IoError::Parse {
        line: 0,
        message: format!("ASCII STL is not valid UTF-8: {e}"),
    })?;
    let mut corners = Vec::new();
    let mut digits = 0u8;
    for (i, line) in text.lines().enumerate() {
        if i.is_multiple_of(TICK_INTERVAL as usize) {
            parsed(ctx, text, line)?;
        }
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
    ctx.progress.report(READING, 1.0);
    Ok((
        weld(&corners, ctx)?,
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
fn weld(corners: &[[f64; 3]], ctx: &RunContext) -> Result<Mesh, IoError> {
    // `+ 0.0` folds -0.0 into 0.0 so they weld together.
    let key = |p: &[f64; 3]| p.map(|c| (c + 0.0).to_bits());
    ctx.progress.report(WELDING, 0.0);
    let mut order = indices(corners.len() as u32, ctx)?;
    // (key, corner) is a total order, so this is deterministic.
    par_sort_unstable_by_key(&mut order, ctx, |&i| (key(&corners[i as usize]), i))?;
    // Group leaders (min corner index) in sorted order.
    let span = ctx.span(WELDING, 0.6, 0.8, order.len());
    let mut leader = vec![0u32; corners.len()];
    let mut groups: Vec<u32> = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        span.at(k)?;
        if k == 0 || key(&corners[order[k - 1] as usize]) != key(&corners[i as usize]) {
            groups.push(i);
        }
        leader[i as usize] = *groups.last().unwrap_or(&i);
    }
    par_sort_unstable_by_key(&mut groups, ctx, |&g| g)?;
    let span = ctx.span(WELDING, 0.85, 0.9, groups.len());
    let mut id_of_leader = vec![u32::MAX; corners.len()];
    let mut positions = Vec::with_capacity(groups.len());
    for (id, &g) in groups.iter().enumerate() {
        span.at(id)?;
        id_of_leader[g as usize] = id as u32;
        positions.push(corners[g as usize].map(|c| c + 0.0));
    }
    let span = ctx.span(WELDING, 0.9, 0.95, corners.len() / 3);
    let mut triangles = Vec::with_capacity(corners.len() / 3);
    for f in 0..corners.len() / 3 {
        span.at(f)?;
        triangles.push([0, 1, 2].map(|k| id_of_leader[leader[f * 3 + k] as usize]));
    }
    finish(positions, triangles, ctx)
}

fn facet_normal(m: &Mesh, t: [u32; 3]) -> [f32; 3] {
    let p = |i: u32| m.corner_position(i);
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
    if let Some((VertexId(v), _)) = mesh
        .vertices()
        .find(|(_, p)| p.iter().any(|&c| !(c as f32).is_finite()))
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
    for (_, t) in mesh.faces() {
        for c in facet_normal(mesh, t) {
            out.extend_from_slice(&c.to_le_bytes());
        }
        for i in t {
            for c in mesh.corner_position(i) {
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
    for (_, t) in mesh.faces() {
        let n = facet_normal(mesh, t);
        // allow-swallow: writing to a String can't fail
        let _ = writeln!(
            s,
            "  facet normal {} {} {}\n    outer loop",
            n[0], n[1], n[2]
        );
        for i in t {
            let p = mesh.corner_position(i);
            // allow-swallow: writing to a String can't fail
            let _ = writeln!(s, "      vertex {} {} {}", p[0], p[1], p[2]);
        }
        s.push_str("    endloop\n  endfacet\n");
    }
    s.push_str("endsolid meshfixer\n");
    s
}
