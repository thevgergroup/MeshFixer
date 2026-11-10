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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
