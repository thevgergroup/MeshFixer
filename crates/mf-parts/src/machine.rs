//! Machine profiles (owner requirements 2026-11-11, "fits: tolerance and
//! clearance are different properties"): how far a printed dimension
//! deviates from the design, measured and entered by the user, per
//! printer. Nothing is assumed: a profile the user hasn't calibrated
//! says so, and every value defaults to 0 (no compensation) until they
//! measure it with a calibration print.
//!
//! This is deliberately separate from [`crate::FitClass`] (the *design
//! intent* gap between mating parts): a generated feature's size comes
//! from a fit class's clearance plus a machine profile's tolerance
//! compensation (see [`crate::resolve`]), and both numbers are recorded
//! on the operation, so replay never reads either profile.
//!
//! Saved to `<config dir>/meshfixer/machine-profiles.yaml`
//! ([`MACHINE_PROFILES_FILE`]), the same shape and rules as fit profiles
//! (load/save/duplicate/rename/delete; a built-in, uncalibrated profile
//! that's always first and can't be edited).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MACHINE_PROFILES_FILE: &str = "machine-profiles.yaml";

/// The built-in, uncalibrated profile's name.
pub const UNCALIBRATED_NAME: &str = "Uncalibrated";

/// One printer's measured tolerances, in mm. Each is a signed deviation
/// of the printed dimension from the design: positive means the print
/// comes out larger than designed (so a hole needs widening by this much
/// to come out the intended size; a boss needs shrinking).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    pub name: String,
    /// True once the user has run a calibration print and entered real
    /// numbers; false means every value below is 0 (D4: no silent
    /// assumption of a tolerance that was never measured).
    pub calibrated: bool,
    /// Outer X/Y dimensions: printed size minus designed size, mm.
    pub outer_xy: f64,
    /// Holes print this much undersize (so a hole's wall closes in by
    /// this much from the design), mm. Subtracted when sizing a bore so
    /// it ends up the intended diameter.
    pub hole_undersize: f64,
    /// Z (layer-stacking direction): printed size minus designed size, mm.
    pub z: f64,
}

impl MachineProfile {
    /// "Uncalibrated": every compensation is 0, so a feature sized from
    /// this profile is exactly the fit class's own clearance, no more.
    pub fn uncalibrated() -> MachineProfile {
        MachineProfile {
            name: UNCALIBRATED_NAME.into(),
            calibrated: false,
            outer_xy: 0.0,
            hole_undersize: 0.0,
            z: 0.0,
        }
    }

    /// The fields by name, with a one-line meaning each (for Settings and
    /// the CLI).
    pub fn fields(&self) -> [(&'static str, f64, &'static str); 3] {
        [
            (
                "outer_xy",
                self.outer_xy,
                "Outer X/Y: printed size minus designed size",
            ),
            (
                "hole_undersize",
                self.hole_undersize,
                "Holes print this much undersize",
            ),
            ("z", self.z, "Z: printed size minus designed size"),
        ]
    }

    fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("a machine profile has an empty name".into());
        }
        for (field, value, _) in self.fields() {
            if !value.is_finite() {
                return Err(format!(
                    "machine profile '{}': {field} is {value}; it must be a finite length in mm",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MachineError {
    #[error("can't read the machine profiles in {path}: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    #[error("the machine profiles in {path} aren't valid: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("can't save the machine profiles to {path}: {reason}")]
    Unwritable { path: PathBuf, reason: String },
    #[error("there's no machine profile named '{0}'")]
    NoSuchProfile(String),
    #[error("the built-in profile can't be changed; duplicate it first")]
    BuiltinIsReadOnly,
    #[error("a machine profile named '{0}' already exists")]
    NameTaken(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MachineProfiles {
    pub active: String,
    pub profiles: Vec<MachineProfile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    active: String,
    #[serde(default)]
    profiles: Vec<MachineProfile>,
}

impl Default for MachineProfiles {
    fn default() -> Self {
        MachineProfiles {
            active: UNCALIBRATED_NAME.into(),
            profiles: vec![MachineProfile::uncalibrated()],
        }
    }
}

impl MachineProfiles {
    pub fn load(dir: &Path) -> Result<MachineProfiles, MachineError> {
        let path = dir.join(MACHINE_PROFILES_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MachineProfiles::default());
            }
            Err(e) => {
                return Err(MachineError::Unreadable {
                    path,
                    reason: e.to_string(),
                });
            }
        };
        let malformed = |reason: String| MachineError::Malformed {
            path: path.clone(),
            reason,
        };
        let file: File = serde_saphyr::from_str(&text).map_err(|e| malformed(e.to_string()))?;
        let mut out = MachineProfiles {
            active: file.active,
            profiles: vec![MachineProfile::uncalibrated()],
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

    pub fn save(&self, dir: &Path) -> Result<(), MachineError> {
        let path = dir.join(MACHINE_PROFILES_FILE);
        let unwritable = |reason: String| MachineError::Unwritable {
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

    pub fn get(&self, name: &str) -> Option<&MachineProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn active_profile(&self) -> &MachineProfile {
        self.get(&self.active)
            .expect("the active profile is always one of the profiles")
    }

    pub fn set_active(&mut self, name: &str) -> Result<(), MachineError> {
        self.get(name)
            .ok_or_else(|| MachineError::NoSuchProfile(name.into()))?;
        self.active = name.into();
        Ok(())
    }

    pub fn duplicate(&mut self, from: &str, new_name: &str) -> Result<(), MachineError> {
        let mut p = self
            .get(from)
            .ok_or_else(|| MachineError::NoSuchProfile(from.into()))?
            .clone();
        if self.get(new_name).is_some() {
            return Err(MachineError::NameTaken(new_name.into()));
        }
        p.name = new_name.into();
        p.check().map_err(MachineError::Invalid)?;
        self.profiles.push(p);
        Ok(())
    }

    pub fn update(&mut self, profile: MachineProfile) -> Result<(), MachineError> {
        profile.check().map_err(MachineError::Invalid)?;
        let p = self.user_profile_mut(&profile.name)?;
        *p = profile;
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), MachineError> {
        self.user_profile_mut(name)?;
        self.profiles.retain(|p| p.name != name);
        if self.active == name {
            self.active = UNCALIBRATED_NAME.into();
        }
        Ok(())
    }

    fn user_profile_mut(&mut self, name: &str) -> Result<&mut MachineProfile, MachineError> {
        if name == UNCALIBRATED_NAME {
            return Err(MachineError::BuiltinIsReadOnly);
        }
        self.profiles
            .iter_mut()
            .find(|p| p.name == name)
            .ok_or_else(|| MachineError::NoSuchProfile(name.into()))
    }
}
