#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Hostile input: declared sizes that would blow memory or hang must fail
//! fast with a structured error, never panic, before the reader allocates
//! for them (MF-IO-002, review focus 5).

use std::io::Write;

use mf_io::{IoError, Limits, obj, stl, threemf};
use mf_runtime::RunContext;

fn zip_with_entry(name: &str, content: &[u8]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    z.start_file("[Content_Types].xml", opt).unwrap();
    z.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#).unwrap();
    z.start_file("_rels/.rels", opt).unwrap();
    z.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#).unwrap();
    z.start_file(name, opt).unwrap();
    z.write_all(content).unwrap();
    z.finish().unwrap();
    buf.into_inner()
}

/// A 3MF whose model part decompresses far beyond its declared or
/// plausible size (a zip bomb): a highly repetitive vertex line, repeated
/// enough to exceed a small `max_model_part_bytes` once inflated, while
/// compressing to a tiny archive. Must fail fast (`LimitExceeded`) with
/// bounded memory, never attempt to hold the fully inflated XML.
///
/// A tight limit (not the 4 GiB default) keeps this test fast and light:
/// the production default is exercised by `stl_declaring_4_billion_triangles_fails_fast`'s
/// sibling checks and the fuzz corpus; what this test proves is that the
/// bound is enforced *during* decompression, before the bomb is fully
/// materialised, which doesn't depend on which limit value is configured.
#[test]
fn threemf_decompression_is_bounded() {
    let line = b"<vertex x=\"0\" y=\"0\" z=\"0\"/>\n";
    let limits = Limits {
        max_model_part_bytes: 1 << 20, // 1 MiB, to keep the bomb small and fast
        ..Limits::default()
    };
    // One repeated line, enough copies to blow well past the 1 MiB cap
    // once decompressed; deflate crushes this to well under 1 KB.
    let repeats = (limits.max_model_part_bytes / line.len() as u64) + 1024;
    let mut model_xml = Vec::from(&b"<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\"><resources><object id=\"1\"><mesh><vertices>\n"[..]);
    for _ in 0..repeats {
        model_xml.extend_from_slice(line);
    }
    model_xml.extend_from_slice(b"</vertices><triangles/></mesh></object></resources><build><item objectid=\"1\"/></build></model>");

    let archive = zip_with_entry("3D/3dmodel.model", &model_xml);
    // The archive itself must be tiny: that's what makes it a zip bomb.
    assert!(
        archive.len() < 10 * 1024,
        "archive is {} bytes; not a meaningful bomb",
        archive.len()
    );
    drop(model_xml);

    let err = threemf::read_with_limits(&archive, &RunContext::none(), limits).unwrap_err();
    assert!(
        matches!(err, IoError::LimitExceeded { .. }),
        "expected LimitExceeded, got {err:?}"
    );
}

/// A 3MF model part within the byte cap, but whose compression ratio is
/// implausibly high, must also be rejected before being fully decompressed.
#[test]
fn threemf_excessive_compression_ratio_is_bounded() {
    let line = b"0000000000000000000000000000\n";
    let limits = Limits {
        max_model_part_bytes: 64 << 20, // comfortably above this test's inflated size
        max_compression_ratio: 50,
        ..Limits::default()
    };
    // A single-byte-varying line compresses extremely well; enough repeats
    // pushes the ratio over the (tightened) limit while staying far under
    // the (also tightened, but much larger) byte cap.
    let repeats = 200_000;
    let mut model_xml = Vec::from(&b"<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\"><resources><object id=\"1\"><mesh><vertices>\n<!--"[..]);
    for _ in 0..repeats {
        model_xml.extend_from_slice(line);
    }
    model_xml.extend_from_slice(b"--></vertices><triangles/></mesh></object></resources><build><item objectid=\"1\"/></build></model>");
    assert!((model_xml.len() as u64) < limits.max_model_part_bytes);

    let archive = zip_with_entry("3D/3dmodel.model", &model_xml);
    assert!(
        (archive.len() as u64) * limits.max_compression_ratio < model_xml.len() as u64,
        "archive {} bytes doesn't exceed a {}x ratio over {} inflated bytes",
        archive.len(),
        limits.max_compression_ratio,
        model_xml.len()
    );

    let err = threemf::read_with_limits(&archive, &RunContext::none(), limits).unwrap_err();
    assert!(
        matches!(err, IoError::LimitExceeded { .. }),
        "expected LimitExceeded, got {err:?}"
    );
}

/// A binary STL header declaring ~4 billion triangles (far beyond
/// `Limits::default().max_triangles`) must fail fast, named by the limit,
/// never attempt to allocate for 4 billion facets. The file itself stays
/// small: only the header's facet count lies.
#[test]
fn stl_declaring_4_billion_triangles_fails_fast() {
    let mut bytes = vec![0u8; 84];
    bytes[..5].copy_from_slice(b"mesh\0");
    bytes[80..84].copy_from_slice(&4_000_000_000u32.to_le_bytes());
    // No facet bytes follow: a genuine file this size doesn't exist, but
    // the limit must be checked (and must fail) before the reader tries to
    // size anything against the (absent) body.
    let err = stl::read(&bytes).unwrap_err();
    assert!(
        matches!(err, IoError::LimitExceeded { .. }),
        "expected LimitExceeded, got {err:?}"
    );
}

/// An OBJ face line with a huge (but syntactically valid) vertex index must
/// fail fast as a named error, never attempt to size a vertex array to it.
#[test]
fn obj_with_huge_indices_fails_fast() {
    let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 4000000000\n";
    let err = obj::read(src).unwrap_err();
    assert!(
        matches!(err, IoError::Parse { .. } | IoError::LimitExceeded { .. }),
        "expected a structured error, got {err:?}"
    );
}

/// Every strict prefix (truncation point) of a representative seed file,
/// for each format, must come back as a structured error, never panic.
#[test]
fn truncated_inputs_never_panic() {
    let stl_bytes = stl::write_binary(&mf_fixtures::cube(1.0)).unwrap();
    for len in 0..stl_bytes.len() {
        let _ = stl::read(&stl_bytes[..len]);
    }

    let obj_text = obj::write(&mf_fixtures::cube(1.0));
    for len in 0..=obj_text.len() {
        // Truncation can land mid UTF-8 character; that's handled upstream
        // by read_model_with's UTF-8 check, so clamp to a valid boundary
        // here (obj::read itself takes &str).
        let mut l = len;
        while l > 0 && !obj_text.is_char_boundary(l) {
            l -= 1;
        }
        let _ = obj::read(&obj_text[..l]);
    }

    let model = threemf::Model {
        unit: threemf::Unit::Millimeter,
        objects: vec![threemf::ModelObject {
            name: "o".into(),
            mesh: mf_fixtures::cube(1.0),
            transform: threemf::IDENTITY,
        }],
    };
    let threemf_bytes = threemf::write(&model).unwrap();
    for len in 0..threemf_bytes.len() {
        let _ = threemf::read(&threemf_bytes[..len]);
    }
}
