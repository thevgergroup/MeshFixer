//! Brush falloff curves (MF-SCULPT-001): weight by distance from the dab
//! centre, as a fraction `t` of the radius. 1 at the centre, 0 from the
//! rim out.

/// How a brush's effect fades from its centre to its rim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Falloff {
    /// Smoothstep: soft centre and soft rim.
    Smooth,
    /// A hemisphere's profile: full until near the rim.
    Sphere,
    /// Quadratic: concentrated at the centre.
    Sharp,
    /// Full strength everywhere inside the radius.
    Flat,
}

impl Falloff {
    /// The weight at `t` = distance / radius: 0 for `t ≥ 1` (and for a
    /// non-finite `t`).
    pub fn weight(self, t: f64) -> f64 {
        if t.is_nan() || t >= 1.0 {
            return 0.0;
        }
        let t = t.max(0.0);
        match self {
            Falloff::Smooth => {
                let u = 1.0 - t;
                u * u * (3.0 - 2.0 * u)
            }
            Falloff::Sphere => (1.0 - t * t).sqrt(),
            Falloff::Sharp => (1.0 - t) * (1.0 - t),
            Falloff::Flat => 1.0,
        }
    }

    /// The spec's word for it.
    pub fn parse(word: &str) -> Option<Falloff> {
        Some(match word {
            "smooth" => Falloff::Smooth,
            "sphere" => Falloff::Sphere,
            "sharp" => Falloff::Sharp,
            "flat" => Falloff::Flat,
            _ => return None,
        })
    }
}
