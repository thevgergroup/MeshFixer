#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Declarative geometry tests (MF-SPEC-004): `tests/geometry/*.yaml` cases
//! of the form given → (select) → when → expect.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use mf_geometry::{FaceId, Mesh, Provenance, inspect};
use mf_operations::{Target, find};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    given: Given,
    #[serde(default)]
    select: Option<Select>,
    when: When,
    expect: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Given {
    fixture: Option<String>,
    file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Select {
    loop_containing_vertex: Option<u32>,
    component_containing_face: Option<u32>,
    faces: Option<Vec<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct When {
    op: String,
    #[serde(default)]
    params: BTreeMap<String, Value>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Named meshes for `given.fixture`.
fn fixture(name: &str) -> Result<Mesh, String> {
    use mf_fixtures::*;
    let grid_without = |n: u32, cells: &[(u32, u32, bool)]| {
        let mut e = grid(n).edit();
        for &(i, j, upper) in cells {
            e.remove_face(FaceId(2 * (j * n + i) + upper as u32))
                .unwrap();
        }
        e.finish().unwrap().0
    };
    Ok(match name {
        "cube" => cube(1.0),
        "cube_with_missing_face" => cube_with_missing_face(),
        "cube_and_two_tetrahedra" => merge(&[
            cube(1.0),
            translated(&tetrahedron(), [5.0, 0.0, 0.0]),
            translated(&tetrahedron(), [9.0, 0.0, 0.0]),
        ]),
        // 6×6 grid, triangular holes at cells (1,1) and (4,4).
        "two_holes_grid" => grid_without(6, &[(1, 1, false), (4, 4, false)]),
        // 4×4 grid, two triangular holes touching only at vertex (2,2) = 12.
        "pinched_holes_grid" => grid_without(4, &[(1, 1, false), (2, 2, true)]),
        other => return Err(format!("unknown fixture '{other}'")),
    })
}

fn target(m: &Mesh, select: &Option<Select>) -> Result<Target, String> {
    let Some(s) = select else {
        return Ok(Target::WholeObject);
    };
    let topo = m.topology();
    if let Some(v) = s.loop_containing_vertex {
        let loops: Vec<u32> = topo
            .boundary_loops
            .iter()
            .filter(|l| l.vertices.contains(&v))
            .map(|l| l.id)
            .collect();
        return match loops.as_slice() {
            [one] => Ok(Target::Loops(vec![*one])),
            [] => Err(format!("no boundary loop contains vertex {v}")),
            many => Err(format!(
                "vertex {v} is on several loops {many:?}; pick a vertex on only one"
            )),
        };
    }
    if let Some(f) = s.component_containing_face {
        let c = topo
            .components
            .iter()
            .position(|c| c.contains(&f))
            .ok_or(format!("face {f} is not live"))?;
        return Ok(Target::Components(vec![c as u32]));
    }
    if let Some(faces) = &s.faces {
        return Ok(Target::Faces(faces.iter().map(|&f| FaceId(f)).collect()));
    }
    Err("empty select".into())
}

fn untouched_outside_target(before: &Mesh, after: &Mesh, prov: &Provenance) -> bool {
    let removed: BTreeSet<FaceId> = prov.removed_faces.iter().copied().collect();
    let modified: BTreeSet<FaceId> = prov.modified_faces.iter().copied().collect();
    before.faces().all(|(id, tri)| {
        removed.contains(&id) || modified.contains(&id) || after.face(id) == Some(tri)
    }) && before.vertices().all(|(id, p)| {
        prov.removed_vertices.contains(&id)
            || prov.moved_vertices.contains(&id)
            || after.position(id).map(|q| q.map(f64::to_bits)) == Some(p.map(f64::to_bits))
    }) && prov.moved_vertices.is_empty()
        && prov.modified_faces.is_empty()
}

fn provenance_total(before: &Mesh, after: &Mesh, prov: &Provenance) -> bool {
    let created: BTreeSet<FaceId> = prov.created_faces.iter().map(|(f, _)| *f).collect();
    let removed: BTreeSet<FaceId> = prov.removed_faces.iter().copied().collect();
    after
        .faces()
        .all(|(id, tri)| (before.face(id) == Some(tri)) ^ created.contains(&id))
        && before
            .faces()
            .all(|(id, _)| after.face(id).is_some() ^ removed.contains(&id))
}

/// Run one case; return the list of mismatches (empty = pass).
fn run_case(case: &Case) -> Result<Vec<String>, String> {
    let before = match (&case.given.fixture, &case.given.file) {
        (Some(name), None) => fixture(name)?,
        (None, Some(file)) => {
            mf_io::read_path(&workspace_root().join(file))
                .map_err(|e| e.to_string())?
                .mesh
        }
        _ => return Err("given needs exactly one of fixture or file".into()),
    };
    let op = find(&case.when.op).ok_or(format!("unknown op '{}'", case.when.op))?;
    let spec = mf_spec::builtin::get(&case.when.op).ok_or("no spec")?;
    let raw: BTreeMap<String, String> = case
        .when
        .params
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string().trim_matches('"').to_owned()))
        .collect();
    let params = spec.validate(&raw).map_err(|e| e.to_string())?;
    let target = target(&before, &case.select)?;
    let result = op.execute(&before, &target, &params);

    let mut problems = Vec::new();
    if let Some(Value::String(code)) = case.expect.get("error") {
        match result {
            Err(mf_operations::OpError::Failed { code: got, .. }) if got == code => {}
            other => problems.push(format!(
                "error: expected '{code}', got {:?}",
                other.map(|o| o.report)
            )),
        }
        return Ok(problems);
    }
    let output = result.map_err(|e| format!("operation failed: {e}"))?;
    let (after, prov) = output
        .change
        .clone()
        .unwrap_or((before.clone(), Provenance::default()));
    let diag = serde_json::to_value(inspect(&after)).unwrap();
    for (key, want) in &case.expect {
        let got: Value = match key.as_str() {
            "untouched_outside_target" => {
                Value::Bool(untouched_outside_target(&before, &after, &prov))
            }
            "provenance_total" => Value::Bool(provenance_total(&before, &after, &prov)),
            "changed" => Value::Bool(output.change.is_some()),
            "volume_delta_pct_lt" => {
                let v0 = inspect(&before)
                    .signed_volume
                    .ok_or("input has no volume")?;
                let v1 = diag["signed_volume"]
                    .as_f64()
                    .ok_or("output has no volume")?;
                let pct = 100.0 * (v1 - v0).abs() / v0.abs();
                Value::Bool(pct < want.as_f64().ok_or("volume_delta_pct_lt needs a number")?)
            }
            k => match diag.get(k) {
                Some(Value::Array(a)) => Value::from(a.len()),
                Some(v) => v.clone(),
                None => return Err(format!("unknown expect key '{k}'")),
            },
        };
        let ok = match (key.as_str(), want) {
            ("volume_delta_pct_lt", _) => got == Value::Bool(true),
            (_, Value::Number(w)) => got
                .as_f64()
                .is_some_and(|g| (g - w.as_f64().unwrap()).abs() <= 1e-9),
            (_, w) => &got == w,
        };
        if !ok {
            problems.push(format!("{key}: expected {want}, got {got}"));
        }
    }
    Ok(problems)
}

fn load(path: &Path) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_saphyr::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn case_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(workspace_root().join("tests/geometry"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    files.sort();
    files
}

#[test]
fn all_declarative_cases_pass() {
    let files = case_files();
    assert!(!files.is_empty(), "no cases in tests/geometry");
    let mut failures = Vec::new();
    let mut count = 0;
    for file in &files {
        for case in load(file).unwrap() {
            count += 1;
            match run_case(&case) {
                Ok(p) if p.is_empty() => {}
                Ok(p) => failures.push(format!(
                    "{} › {}: {}",
                    file.display(),
                    case.name,
                    p.join("; ")
                )),
                Err(e) => failures.push(format!("{} › {}: {e}", file.display(), case.name)),
            }
        }
    }
    eprintln!("{count} declarative cases in {} files", files.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn cases_validate_against_schema() {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_root().join("specs/geometry-test.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for file in case_files() {
        let value: Value =
            serde_saphyr::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{}: {errors:?}", file.display());
    }
}

fn parse(yaml: &str) -> Case {
    let mut cases: Vec<Case> = serde_saphyr::from_str(yaml).unwrap();
    cases.remove(0)
}

#[test]
fn a_failing_case_reports_its_key() {
    let case = parse(
        "- name: wrong\n  given: { fixture: cube }\n  when: { op: mesh.inspect }\n  expect: { components: 2 }\n",
    );
    let problems = run_case(&case).unwrap();
    assert_eq!(problems, vec!["components: expected 2, got 1".to_string()]);
}

#[test]
fn unknown_expect_key_is_an_error() {
    let case = parse(
        "- name: typo\n  given: { fixture: cube }\n  when: { op: mesh.inspect }\n  expect: { componnets: 1 }\n",
    );
    assert!(
        run_case(&case)
            .unwrap_err()
            .contains("unknown expect key 'componnets'")
    );
}
