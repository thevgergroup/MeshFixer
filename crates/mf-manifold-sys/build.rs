//! Builds the C++ Manifold library (elalish/manifold, Apache-2.0) and our
//! C ABI shim over it (`shim/shim.cpp`), statically linked.
//!
//! - Source: `$MF_MANIFOLD_SRC` if set (an offline checkout, e.g. a CI
//!   cache), else a shallow clone of the pinned tag into `OUT_DIR`, checked
//!   against the pinned commit.
//! - Parallel backend: oneTBB (Apache-2.0), fetched and built statically by
//!   Manifold's own CMake (`MANIFOLD_USE_BUILTIN_TBB`, pinned by Manifold to
//!   v2022.3.0).
//! - Always a Release build (the cmake crate would otherwise follow the
//!   cargo profile, and a Debug Manifold measures the build, not the kernel).
//! - Build parallelism follows cargo's `-j` (the cmake crate passes
//!   `NUM_JOBS` as `--parallel`).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFOLD_TAG: &str = "v3.5.3";
const MANIFOLD_COMMIT: &str = "0edd9d54876f3135e431575214dd6d8a72866fee";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("could not run git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn fetch(out: &Path) -> PathBuf {
    let src = out.join("manifold-src");
    if !src.join("CMakeLists.txt").is_file() {
        if src.exists() {
            std::fs::remove_dir_all(&src).expect("could not clear a partial Manifold checkout");
        }
        git(
            out,
            &[
                "-c",
                "advice.detachedHead=false",
                "clone",
                "--quiet",
                "--depth",
                "1",
                "--branch",
                MANIFOLD_TAG,
                "https://github.com/elalish/manifold.git",
                src.to_str().expect("OUT_DIR is not UTF-8"),
            ],
        );
    }
    let head = git(&src, &["rev-parse", "HEAD"]);
    assert_eq!(
        head, MANIFOLD_COMMIT,
        "Manifold {MANIFOLD_TAG} is not the pinned commit"
    );
    src
}

/// The directory holding a static library called `name`, searched below `dir`.
fn find_lib(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidates = [
        format!("lib{name}.a"),
        format!("{name}.lib"),
        format!("lib{name}_static.a"),
    ];
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_lib(&path, name) {
                return Some(found);
            }
        } else if let Some(file) = path.file_name().and_then(|f| f.to_str())
            && candidates.iter().any(|c| c == file)
        {
            return Some(dir.to_path_buf());
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=shim/shim.cpp");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=MF_MANIFOLD_SRC");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let src = match env::var_os("MF_MANIFOLD_SRC") {
        Some(dir) => PathBuf::from(dir),
        None => fetch(&out),
    };

    let dst = cmake::Config::new(&src)
        .profile("Release")
        .define("MANIFOLD_PAR", "ON")
        .define("MANIFOLD_USE_BUILTIN_TBB", "ON")
        .define("MANIFOLD_CROSS_SECTION", "OFF")
        .define("MANIFOLD_CBIND", "OFF")
        .define("MANIFOLD_TEST", "OFF")
        .define("MANIFOLD_PYBIND", "OFF")
        .define("MANIFOLD_JSBIND", "OFF")
        .define("MANIFOLD_NO_IOSTREAM", "ON")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .define("TBB_TEST", "OFF")
        .define("TBB_EXAMPLES", "OFF")
        .build_target("manifold")
        .build();
    let build = dst.join("build");

    let tbb_include = build.join("_deps/tbb-src/include");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("shim/shim.cpp")
        .include(src.join("include"))
        .include(build.join("include"))
        .include(&tbb_include)
        .define("MANIFOLD_PAR", "1")
        .define("MANIFOLD_NO_IOSTREAM", None)
        .define("MANIFOLD_NO_FILESYSTEM", None)
        .opt_level(3)
        .debug(false)
        .warnings(false)
        .compile("mf_manifold_shim");

    let manifold = find_lib(&build, "manifold").expect("libmanifold was not built");
    println!("cargo:rustc-link-search=native={}", manifold.display());
    println!("cargo:rustc-link-lib=static=manifold");
    let tbb = ["tbb", "tbb12", "tbb12_static"]
        .into_iter()
        .find_map(|name| find_lib(&build, name).map(|dir| (dir, name)));
    let (dir, name) = tbb.expect("oneTBB was not built");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static={name}");
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
    } else if !target.contains("msvc") {
        println!("cargo:rustc-link-lib=stdc++");
    }
}
