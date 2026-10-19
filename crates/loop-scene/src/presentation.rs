//! Scene-authored lighting, independent of domain and geometry allocation.
use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Environment {
    pub purpose: String,
    /// `physical_sky` uses atmospheric scattering; `constant` is a uniform world.
    pub sky: String,
    pub background_color: [f64; 3],
    pub strength: f64,
    pub exposure: f64,
    #[serde(default)]
    pub sun: Option<Sun>,
    #[serde(default)]
    pub lights: Vec<Light>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sun {
    pub elevation_degrees: f64,
    pub azimuth_degrees: f64,
    /// Direct-light irradiance; sky scattering strength is separate.
    pub energy: f64,
    pub color: [f64; 3],
    pub angular_size_degrees: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Light {
    pub id: String,
    pub purpose: String,
    pub kind: String,
    pub position: [f64; 3],
    pub target: [f64; 3],
    pub color: [f64; 3],
    pub power_w: f64,
    pub size_m: f64,
    #[serde(default = "spot_angle")]
    pub spot_angle_degrees: f64,
}
fn spot_angle() -> f64 {
    45.0
}
fn between(v: f64, lo: f64, hi: f64) -> bool {
    v.is_finite() && (lo..=hi).contains(&v)
}
fn color(c: &[f64; 3]) -> bool {
    c.iter().all(|v| between(*v, 0.0, 1.0))
}
impl Environment {
    pub(crate) fn validate(&self, max_extent: f64) -> Result<()> {
        let invalid = || {
            SceneError::Validation("invalid authored environment: finite colors, energies, metre positions, unique light IDs and purposeful lighting required".into())
        };
        if self.purpose.trim().is_empty()
            || !matches!(self.sky.as_str(), "constant" | "physical_sky")
            || !color(&self.background_color)
            || !between(self.strength, 0.0, 10.0)
            || !between(self.exposure, -16.0, 16.0)
            || self.lights.len() > 64
        {
            return Err(invalid());
        }
        if self.sky == "physical_sky" && self.sun.is_none() {
            return Err(invalid());
        }
        if let Some(sun) = &self.sun {
            if !between(sun.elevation_degrees, 0.0, 90.0)
                || !between(sun.azimuth_degrees, -360.0, 360.0)
                || !between(sun.energy, 0.0, 100.0)
                || !color(&sun.color)
                || !between(sun.angular_size_degrees, 0.01, 45.0)
            {
                return Err(invalid());
            }
        }
        let mut ids = HashSet::new();
        for light in &self.lights {
            if light.id.is_empty()
                || light.id.len() > 64
                || !light
                    .id
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'-')
                || !ids.insert(&light.id)
                || light.purpose.trim().is_empty()
                || !matches!(light.kind.as_str(), "area" | "point" | "spot")
                || light
                    .position
                    .iter()
                    .chain(&light.target)
                    .any(|v| !v.is_finite())
                || !color(&light.color)
                || !between(light.power_w, 0.0, 100_000_000.0)
                || !between(light.size_m, 0.001, max_extent)
                || !between(light.spot_angle_degrees, 1.0, 179.0)
                || (light.kind != "point"
                    && (0..3).all(|i| (light.position[i] - light.target[i]).abs() < 0.001))
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_environments_validate_without_room_or_sun_assumptions() {
        let mut environment: Environment = serde_json::from_value(serde_json::json!({
            "purpose":"spotlit exhibition after dark", "sky":"constant", "background_color":[0.01,0.01,0.01],
            "strength":0.1,"exposure":0,"lights":[{"id":"exhibit","purpose":"illuminate joinery", "kind":"spot",
            "position":[0,-2,4],"target":[0,0,1],"color":[1,0.9,0.8],"power_w":500,"size_m":0.1}]
        })).unwrap();
        environment.validate(1000.0).unwrap();
        environment.lights.push(environment.lights[0].clone());
        assert!(environment.validate(1000.0).is_err());
        environment.lights.pop();
        environment.sky = "physical_sky".into();
        assert!(environment.validate(1000.0).is_err());
        environment.sky = "constant".into();
        environment.lights[0].power_w = f64::NAN;
        assert!(environment.validate(1000.0).is_err());
    }
}
