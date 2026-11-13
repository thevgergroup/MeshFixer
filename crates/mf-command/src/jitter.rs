//! Test-only position jitter (MF-ISSUE-046), compiled only with the
//! `test-position-jitter` feature.
//!
//! With `MF_TEST_POSITION_JITTER=<fraction>` set, every coordinate of every
//! vertex an operation creates is scaled by `1 ± fraction` (the sign
//! alternating per vertex), a relative error like floating point's, before
//! the output is hashed and committed. The
//! structure is untouched, so a log recorded this way differs from a normal
//! build's results only in bits, the way another platform's floating point
//! can: replaying it must give `Equivalent`. Unset, nothing changes. A value
//! that isn't a positive finite number panics rather than being ignored.

use mf_operations::OpOutput;

pub(crate) const VAR: &str = "MF_TEST_POSITION_JITTER";

fn fraction() -> Option<f64> {
    let raw = std::env::var_os(VAR)?;
    let raw = raw
        .to_str()
        .unwrap_or_else(|| panic!("{VAR} is not valid UTF-8"));
    let f: f64 = raw
        .parse()
        .unwrap_or_else(|e| panic!("{VAR}={raw} is not a number: {e}"));
    assert!(
        f.is_finite() && f > 0.0,
        "{VAR}={raw} must be a positive finite number"
    );
    Some(f)
}

pub(crate) fn perturb(output: OpOutput) -> OpOutput {
    let Some(f) = fraction() else {
        return output;
    };
    let Some((mesh, prov)) = output.change else {
        return output;
    };
    if prov.created_vertices.is_empty() {
        return OpOutput {
            change: Some((mesh, prov)),
            report: output.report,
        };
    }
    let mut edit = mesh.edit();
    for (k, &v) in prov.created_vertices.iter().enumerate() {
        let p = mesh.position(v).expect("a created vertex is live");
        let scale = if k % 2 == 0 { 1.0 + f } else { 1.0 - f };
        edit.move_vertex(v, p.map(|x| x * scale))
            .expect("moving a live vertex");
    }
    // Moving vertices this command created changes no element sets, so the
    // operation's own provenance still describes the change.
    let (moved, _) = edit.finish().expect("only positions changed");
    OpOutput {
        change: Some((moved, prov)),
        report: output.report,
    }
}
