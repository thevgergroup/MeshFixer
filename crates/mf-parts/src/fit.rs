//! Print fit profiles (D20): every printing tolerance in one reusable
//! place. Operations read their defaults from the active profile through a
//! spec parameter's `default_from: fit.<field>` (filled by the frontend at
//! prepare time, `OperationSpec::fill_defaults`), and record the values
//! they used, so replay never reads a profile.
//!
//! Profiles are saved to `<config dir>/meshfixer/fit-profiles.yaml`
//! ([`FIT_PROFILES_FILE`]). The built-in profile is always first, is never
//! written to the file and can't be edited, renamed or deleted; users
//! duplicate it. A file that doesn't exist yet means "only the built-in";
//! a file that can't be read or parsed is a named error, never ignored.
//!
//! **D20 restructuring (owner requirements 2026-11-11):** [`FitProfile`]
//! mixed two different properties. Tolerance (how far a print deviates
//! from the design) belongs to the *machine*: see [`crate::MachineProfile`].
//! Clearance (the gap between mating parts the design wants) belongs to
//! the *design intent*: [`FitClass`], below. [`FitProfile`] stays, byte-
//! compatible, as the legacy all-in-one shape every Phase 1 task before
//! this one already wrote specs and tests against; new connector work
//! (Tasks 25-28) uses [`FitClass`] plus a [`crate::MachineProfile`],
//! resolved by [`resolve`]. Both kinds of value are recorded on the
//! operation that used them, so replay never reads either profile, and
//! an old log (recorded before this split) replays unchanged.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::MachineProfile;

/// The profiles file's name in the MeshFixer settings folder.
pub const FIT_PROFILES_FILE: &str = "fit-profiles.yaml";

/// The built-in profile's name.
pub const BUILTIN_NAME: &str = "Default FDM, 0.4 mm nozzle";

/// One printer/material's tolerances, in mm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitProfile {
    pub name: String,
    /// A dowel's diameter is its hole's diameter minus this.
    pub peg_clearance: f64,
    /// A dowel is shorter than its two holes together by this.
    pub length_allowance: f64,
    /// The gap around parts printed in place (hinges), and a lid's cut gap.
    pub hinge_gap: f64,
    /// How far a clasp's lip passes under its catch.
    pub clasp_interference: f64,
}

impl FitProfile {
    /// "Default FDM, 0.4 mm nozzle": 0.2 / 0.4 / 0.4 / 0.3 mm.
    pub fn builtin() -> FitProfile {
        FitProfile {
            name: BUILTIN_NAME.into(),
            peg_clearance: 0.2,
            length_allowance: 0.4,
            hinge_gap: 0.4,
            clasp_interference: 0.3,
        }
    }

    /// The fields by their `fit.<field>` names, with a one-line meaning
    /// each (for Settings ▸ Print fit and the CLI).
    pub fn fields(&self) -> [(&'static str, f64, &'static str); 4] {
        [
            (
                "peg_clearance",
                self.peg_clearance,
                "Dowel is this much thinner than its hole",
            ),
            (
                "length_allowance",
                self.length_allowance,
                "Dowel is this much shorter than its two holes together",
            ),
            (
                "hinge_gap",
                self.hinge_gap,
                "Gap around parts printed in place, and a lid's cut gap",
            ),
            (
                "clasp_interference",
                self.clasp_interference,
                "How far a clasp's lip passes under its catch",
            ),
        ]
    }

    fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("a profile has an empty name".into());
        }
        for (field, value, _) in self.fields() {
            if !value.is_finite() || value < 0.0 {
                return Err(format!(
                    "profile '{}': {field} is {value}; it must be a length of 0 mm or more",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

impl mf_spec::FitDefaults for FitProfile {
    fn fit_value(&self, field: &str) -> Option<f64> {
        self.fields()
            .into_iter()
            .find(|(name, _, _)| *name == field)
            .map(|(_, value, _)| value)
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FitError {
    #[error("can't read the print fit profiles in {path}: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    #[error("the print fit profiles in {path} aren't valid: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("can't save the print fit profiles to {path}: {reason}")]
    Unwritable { path: PathBuf, reason: String },
    #[error("there's no print fit profile named '{0}'")]
    NoSuchProfile(String),
    #[error("the built-in profile can't be changed; duplicate it first")]
    BuiltinIsReadOnly,
    #[error("a print fit profile named '{0}' already exists")]
    NameTaken(String),
    #[error("{0}")]
    Invalid(String),
}

/// Every profile, the built-in first, and which one is active.
#[derive(Debug, Clone, PartialEq)]
pub struct FitProfiles {
    pub active: String,
    pub profiles: Vec<FitProfile>,
}

/// The file's form: the user's profiles only.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    active: String,
    #[serde(default)]
    profiles: Vec<FitProfile>,
}

impl Default for FitProfiles {
    fn default() -> Self {
        FitProfiles {
            active: BUILTIN_NAME.into(),
            profiles: vec![FitProfile::builtin()],
        }
    }
}

impl FitProfiles {
    /// The profiles saved in `dir` (`dir/fit-profiles.yaml`; normally
    /// [`crate::dirs::meshfixer_dir`]). No file yet: only the built-in.
    pub fn load(dir: &Path) -> Result<FitProfiles, FitError> {
        let path = dir.join(FIT_PROFILES_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FitProfiles::default());
            }
            Err(e) => {
                return Err(FitError::Unreadable {
                    path,
                    reason: e.to_string(),
                });
            }
        };
        let malformed = |reason: String| FitError::Malformed {
            path: path.clone(),
            reason,
        };
        let file: File = serde_saphyr::from_str(&text).map_err(|e| malformed(e.to_string()))?;
        let mut out = FitProfiles {
            active: file.active,
            profiles: vec![FitProfile::builtin()],
        };
        for p in file.profiles {
            p.check().map_err(malformed)?;
            if out.get(&p.name).is_some() {
                return Err(malformed(format!("two profiles are named '{}'", p.name)));
            }
            out.profiles.push(p);
        }
        if out.get(&out.active).is_none() {
            return Err(malformed(format!(
                "the active profile '{}' isn't one of the profiles",
                out.active
            )));
        }
        Ok(out)
    }

    /// Writes the user's profiles and the active name to
    /// `dir/fit-profiles.yaml`, creating `dir` if needed.
    pub fn save(&self, dir: &Path) -> Result<(), FitError> {
        let path = dir.join(FIT_PROFILES_FILE);
        let unwritable = |reason: String| FitError::Unwritable {
            path: path.clone(),
            reason,
        };
        let file = File {
            active: self.active.clone(),
            profiles: self.profiles.iter().skip(1).cloned().collect(),
        };
        let text = serde_saphyr::to_string(&file).map_err(|e| unwritable(e.to_string()))?;
        std::fs::create_dir_all(dir).map_err(|e| unwritable(e.to_string()))?;
        std::fs::write(&path, text).map_err(|e| unwritable(e.to_string()))
    }

    /// The profile named `name` exactly.
    pub fn get(&self, name: &str) -> Option<&FitProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// The profile a user asked for by name (`--fit-profile`): an exact
    /// name first; otherwise "default" in any case means the built-in
    /// profile ("Default FDM, 0.4 mm nozzle").
    pub fn lookup(&self, name: &str) -> Option<&FitProfile> {
        self.get(name).or_else(|| {
            name.eq_ignore_ascii_case("default")
                .then(|| self.get(BUILTIN_NAME))
                .flatten()
        })
    }

    /// The active profile.
    pub fn active_profile(&self) -> &FitProfile {
        self.get(&self.active)
            .expect("the active profile is always one of the profiles (load, set_active and delete keep it so)")
    }

    pub fn set_active(&mut self, name: &str) -> Result<(), FitError> {
        self.get(name)
            .ok_or_else(|| FitError::NoSuchProfile(name.into()))?;
        self.active = name.into();
        Ok(())
    }

    /// Copies `from` as `new_name` (the way to edit the built-in).
    pub fn duplicate(&mut self, from: &str, new_name: &str) -> Result<(), FitError> {
        let mut p = self
            .get(from)
            .ok_or_else(|| FitError::NoSuchProfile(from.into()))?
            .clone();
        if self.get(new_name).is_some() {
            return Err(FitError::NameTaken(new_name.into()));
        }
        p.name = new_name.into();
        p.check().map_err(FitError::Invalid)?;
        self.profiles.push(p);
        Ok(())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), FitError> {
        if self.get(to).is_some() {
            return Err(FitError::NameTaken(to.into()));
        }
        let p = self.user_profile_mut(from)?;
        p.name = to.into();
        p.check().map_err(FitError::Invalid)?;
        if self.active == from {
            self.active = to.into();
        }
        Ok(())
    }

    /// Deletes a user profile; if it was active, the built-in becomes
    /// active.
    pub fn delete(&mut self, name: &str) -> Result<(), FitError> {
        self.user_profile_mut(name)?;
        self.profiles.retain(|p| p.name != name);
        if self.active == name {
            self.active = BUILTIN_NAME.into();
        }
        Ok(())
    }

    /// Replaces a user profile's values (its name stays).
    pub fn update(&mut self, profile: FitProfile) -> Result<(), FitError> {
        profile.check().map_err(FitError::Invalid)?;
        let p = self.user_profile_mut(&profile.name)?;
        *p = profile;
        Ok(())
    }

    fn user_profile_mut(&mut self, name: &str) -> Result<&mut FitProfile, FitError> {
        if name == BUILTIN_NAME {
            return Err(FitError::BuiltinIsReadOnly);
        }
        self.profiles
            .iter_mut()
            .find(|p| p.name == name)
            .ok_or_else(|| FitError::NoSuchProfile(name.into()))
    }
}

/// A fit class (design intent, owner requirements 2026-11-11): the gap
/// between mating parts a design wants, independent of any one printer's
/// tolerance. `clearance` is a radial/linear gap in mm; negative means
/// interference (a press fit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitClass {
    pub name: String,
    /// The gap (mm) between mating surfaces this class wants, before any
    /// machine tolerance compensation. Negative is interference (press
    /// fit); near 0 is snug/transition; positive is sliding or free
    /// running.
    pub clearance: f64,
}

impl FitClass {
    /// The four classes `connectors-fits-requirements.md` names, in
    /// increasing clearance.
    pub fn builtins() -> [FitClass; 4] {
        [
            FitClass {
                name: "Press".into(),
                clearance: -0.1,
            },
            FitClass {
                name: "Snug".into(),
                clearance: 0.0,
            },
            FitClass {
                name: "Sliding".into(),
                clearance: 0.15,
            },
            FitClass {
                name: "Free running".into(),
                clearance: 0.3,
            },
        ]
    }

    fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("a fit class has an empty name".into());
        }
        if !self.clearance.is_finite() {
            return Err(format!(
                "fit class '{}': clearance {} must be a finite length in mm",
                self.name, self.clearance
            ));
        }
        Ok(())
    }
}

/// Every fit class, the built-ins first, and which one is active
/// (per-use, not global: a hinge and a clasp in the same document can
/// use different classes).
#[derive(Debug, Clone, PartialEq)]
pub struct FitClasses {
    pub classes: Vec<FitClass>,
}

impl Default for FitClasses {
    fn default() -> Self {
        FitClasses {
            classes: FitClass::builtins().into(),
        }
    }
}

const FIT_CLASSES_FILE: &str = "fit-classes.yaml";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassesFile {
    #[serde(default)]
    classes: Vec<FitClass>,
}

impl FitClasses {
    /// Built-ins plus the user's own, from `dir/fit-classes.yaml`. No
    /// file yet: only the built-ins.
    pub fn load(dir: &Path) -> Result<FitClasses, FitError> {
        let path = dir.join(FIT_CLASSES_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FitClasses::default());
            }
            Err(e) => {
                return Err(FitError::Unreadable {
                    path,
                    reason: e.to_string(),
                });
            }
        };
        let malformed = |reason: String| FitError::Malformed {
            path: path.clone(),
            reason,
        };
        let file: ClassesFile =
            serde_saphyr::from_str(&text).map_err(|e| malformed(e.to_string()))?;
        let mut out = FitClasses::default();
        let builtin_names: Vec<String> = out.classes.iter().map(|c| c.name.clone()).collect();
        for c in file.classes {
            c.check().map_err(malformed)?;
            if builtin_names.contains(&c.name) || out.get(&c.name).is_some() {
                return Err(malformed(format!("two fit classes are named '{}'", c.name)));
            }
            out.classes.push(c);
        }
        Ok(out)
    }

    pub fn save(&self, dir: &Path) -> Result<(), FitError> {
        let path = dir.join(FIT_CLASSES_FILE);
        let unwritable = |reason: String| FitError::Unwritable {
            path: path.clone(),
            reason,
        };
        let builtins = FitClass::builtins();
        let file = ClassesFile {
            classes: self
                .classes
                .iter()
                .filter(|c| !builtins.iter().any(|b| b.name == c.name))
                .cloned()
                .collect(),
        };
        let text = serde_saphyr::to_string(&file).map_err(|e| unwritable(e.to_string()))?;
        std::fs::create_dir_all(dir).map_err(|e| unwritable(e.to_string()))?;
        std::fs::write(&path, text).map_err(|e| unwritable(e.to_string()))
    }

    pub fn get(&self, name: &str) -> Option<&FitClass> {
        self.classes.iter().find(|c| c.name == name)
    }
}

/// A feature dimension resolved from design intent plus measured
/// tolerance (owner requirements 2026-11-11): `fit_class`'s clearance,
/// compensated by `machine`'s relevant tolerance. `compensation` picks
/// which of the machine's measured deviations applies (a radial gap
/// around a bore widens by the hole's undersize; an axial/Z gap widens
/// by the Z deviation; a plain XY gap by the outer XY deviation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compensation {
    /// A gap formed by a hole/bore (widen by `hole_undersize`).
    Hole,
    /// A gap along Z (widen by `z`).
    Z,
    /// A plain XY gap between two outer surfaces (widen by `outer_xy`).
    OuterXy,
    /// No machine compensation (the clearance is used as designed).
    None,
}

/// The resolved gap (mm) a generated feature should use: the fit class's
/// clearance plus the machine profile's relevant measured deviation.
/// Both source values are what the caller should record (D20's "the
/// values used are recorded in each operation's parameters").
pub fn resolve(fit_class: &FitClass, machine: &MachineProfile, compensation: Compensation) -> f64 {
    let tolerance = match compensation {
        Compensation::Hole => machine.hole_undersize,
        Compensation::Z => machine.z,
        Compensation::OuterXy => machine.outer_xy,
        Compensation::None => 0.0,
    };
    fit_class.clearance + tolerance
}

/// Fills every parameter of `spec` that declares a `fit_role`
/// (`mf_spec::FIT_ROLES`) and that `raw` doesn't already set, from the
/// chosen `fit_class` and `machine` (parts-architecture-review.md's
/// generic Fit/Machine filler, replacing a connector-by-connector `if
/// id == ...` chain): `"clearance"`/`"hole"`/`"outer_xy"`/`"z"` resolve
/// through [`resolve`] with the matching [`Compensation`];
/// `"machine_hole_undersize"` is `machine.hole_undersize` on its own,
/// uncompensated; `"fit_class_name"`/`"machine_profile_name"` are the
/// chosen pair's own names. A parameter the caller already set in `raw`
/// is left alone (the same "set wins over derived" rule
/// `OperationSpec::fill_defaults` already uses for `default_from`).
pub fn fill_fit_role_params(
    spec: &mf_spec::OperationSpec,
    raw: &mut std::collections::BTreeMap<String, String>,
    fit_class: &FitClass,
    machine: &MachineProfile,
) {
    for p in &spec.parameters {
        let Some(role) = p.fit_role.as_deref() else {
            continue;
        };
        if raw.contains_key(&p.name) {
            continue;
        }
        let value = match role {
            "clearance" => resolve(fit_class, machine, Compensation::None).to_string(),
            "hole" => resolve(fit_class, machine, Compensation::Hole).to_string(),
            "outer_xy" => resolve(fit_class, machine, Compensation::OuterXy).to_string(),
            "z" => resolve(fit_class, machine, Compensation::Z).to_string(),
            "machine_hole_undersize" => machine.hole_undersize.to_string(),
            "fit_class_name" => fit_class.name.clone(),
            "machine_profile_name" => machine.name.clone(),
            // allow-swallow: `mf_spec::OperationSpec::validate` already refused any `fit_role` not in `FIT_ROLES` when the spec was parsed, so an unknown role here can't happen; this is a no-op rather than a panic in case a future role is added to FIT_ROLES without a matching arm here.
            _ => continue,
        };
        raw.insert(p.name.clone(), value);
    }
}
