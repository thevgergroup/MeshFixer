#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The 5M memory gate (MF-GEO-002, MF-ISSUE-019). "5M" is the requested
//! size: the damaged-sphere fixture cuts holes, leaving ≈4.8M triangles
//! (the JSON reports both): the working set (the mesh
//! plus its cached topology) stays within 400 MB. The same command is CI's
//! memory gate job. Heavy (a 5M mesh): run serially, see scripts/run-tests.sh.

use std::process::Command;

/// MF-GEO-002: mesh plus cached topology at 5M requested (≈4.8M actual)
/// triangles.
const WORKING_SET_BUDGET: u64 = 400 << 20;

#[test]
fn working_set_under_400mb_at_5m() {
    let out = Command::new(env!("CARGO_BIN_EXE_meshfixer"))
        .args(["bench", "memory", "--triangles", "5000000", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&v).unwrap());
    assert_eq!(v["allocator_installed"], true, "{v}");
    assert!(v["triangles"].as_u64().unwrap() >= 4_800_000, "{v}");
    let (mesh, topology, working_set) = (
        v["mesh_bytes"].as_u64().unwrap(),
        v["topology_bytes"].as_u64().unwrap(),
        v["working_set_bytes"].as_u64().unwrap(),
    );
    assert!(mesh > 0 && topology > 0, "{v}");
    assert_eq!(working_set, mesh + topology, "{v}");
    assert!(
        v["topology_peak_bytes"].as_u64().unwrap() >= topology,
        "the build peak includes what it keeps: {v}"
    );
    assert!(
        working_set <= WORKING_SET_BUDGET,
        "working set {} MB > {} MB",
        working_set >> 20,
        WORKING_SET_BUDGET >> 20
    );
}
