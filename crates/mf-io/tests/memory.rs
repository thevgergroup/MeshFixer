#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Peak-memory bounds for import and export (MF-ISSUE-013, ADR-0001).
//! One test per binary would be ideal; these run serially via a mutex
//! because the allocator counters are process-wide.

use std::sync::Mutex;

use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_io::{Format, read_path, write_path};
use mf_metrics::{CountingAlloc, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;
static SERIAL: Mutex<()> = Mutex::new(());

fn fixture() -> mf_geometry::Mesh {
    // ~200k triangles
    damaged_sphere(&DamageSpec::for_triangles(200_000, 1))
}

#[test]
fn stl_import_peak_bounded() {
    let _g = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("f.stl");
    write_path(&fixture(), &p, Format::StlBinary).unwrap();
    let file_len = std::fs::metadata(&p).unwrap().len();
    let (imported, phase) = measure("import", || read_path(&p).unwrap());
    let mesh_bytes = imported.mesh.heap_bytes();
    let bound = file_len + mesh_bytes * 5 / 2;
    assert!(
        phase.peak_bytes <= bound,
        "import peak {} > bound {} (file {}, mesh {})",
        phase.peak_bytes,
        bound,
        file_len,
        mesh_bytes
    );
}

#[test]
fn export_peak_bounded() {
    let _g = SERIAL.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mesh = fixture();
    for (name, format) in [("e.stl", Format::StlBinary), ("e.obj", Format::Obj)] {
        let p = dir.path().join(name);
        let ((), phase) = measure("export", || write_path(&mesh, &p, format).unwrap());
        let encoded = std::fs::metadata(&p).unwrap().len();
        let bound = encoded + mesh.heap_bytes() * 3 / 2;
        assert!(
            phase.peak_bytes <= bound,
            "{name}: export peak {} > bound {} (encoded {}, mesh {})",
            phase.peak_bytes,
            bound,
            encoded,
            mesh.heap_bytes()
        );
    }
}
