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
        let mut issues = Vec::new();
        if self.purpose.trim().is_empty() {
            issues.push("environment.purpose must explain the lighting intent".into());
        }
        if !matches!(self.sky.as_str(), "constant" | "physical_sky") {
            issues.push(format!(
                "environment.sky {:?} must be constant or physical_sky",
                self.sky
            ));
        }
        if !color(&self.background_color) {
            issues.push(format!(
                "environment.background_color {:?} must be linear RGB in 0..1",
                self.background_color
            ));
        }
        for (key, value, lo, hi) in [
            ("strength", self.strength, 0.0, 10.0),
            ("exposure", self.exposure, -16.0, 16.0),
        ] {
            if !between(value, lo, hi) {
                issues.push(format!(
                    "environment.{key}={value} must be finite in {lo}..{hi}"
                ));
            }
        }
        if self.lights.len() > 64 {
            issues.push("environment.lights supports at most 64 sources".into());
        }
        if self.sky == "physical_sky" && self.sun.is_none() {
            issues.push("physical_sky requires environment.sun".into());
        }
        if let Some(sun) = &self.sun {
            for (key, value, lo, hi) in [
                ("elevation_degrees", sun.elevation_degrees, 0.0, 90.0),
                ("azimuth_degrees", sun.azimuth_degrees, -360.0, 360.0),
                ("energy", sun.energy, 0.0, 100.0),
                ("angular_size_degrees", sun.angular_size_degrees, 0.01, 45.0),
            ] {
                if !between(value, lo, hi) {
                    issues.push(format!(
                        "environment.sun.{key}={value} must be finite in {lo}..{hi}"
                    ));
                }
            }
            if !color(&sun.color) {
                issues.push(format!(
                    "environment.sun.color {:?} must be linear RGB in 0..1",
                    sun.color
                ));
            }
        }
        let mut ids = HashSet::new();
        for (index, light) in self.lights.iter().enumerate() {
            let path = format!("environment.lights[{index}] ({})", light.id);
            if light.id.is_empty()
                || light.id.len() > 64
                || !light
                    .id
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'-')
                || !ids.insert(&light.id)
            {
                issues.push(format!(
                    "{path}.id must be unique, 1..64 ASCII letters/digits/underscores/hyphens"
                ));
            }
            if light.purpose.trim().is_empty() {
                issues.push(format!("{path}.purpose must explain this source"));
            }
            if !matches!(light.kind.as_str(), "area" | "point" | "spot") {
                issues.push(format!(
                    "{path}.kind {:?} must be area, point or spot",
                    light.kind
                ));
            }
            if light
                .position
                .iter()
                .chain(&light.target)
                .any(|v| !v.is_finite())
            {
                issues.push(format!(
                    "{path}.position and target must be finite WORLD metre coordinates"
                ));
            }
            if !color(&light.color) {
                issues.push(format!(
                    "{path}.color {:?} must be linear RGB in 0..1",
                    light.color
                ));
            }
            for (key, value, lo, hi) in [
                ("power_w", light.power_w, 0.0, 100_000_000.0),
                ("size_m", light.size_m, 0.001, max_extent),
                ("spot_angle_degrees", light.spot_angle_degrees, 1.0, 179.0),
            ] {
                if !between(value, lo, hi) {
                    issues.push(format!("{path}.{key}={value} must be finite in {lo}..{hi}"));
                }
            }
            if light.kind != "point"
                && (0..3).all(|i| (light.position[i] - light.target[i]).abs() < 0.001)
            {
                issues.push(format!(
                    "{path}.target must differ from position for an aimed source"
                ));
            }
        }
        if !issues.is_empty() {
            return Err(SceneError::Validation(format!(
                "invalid authored environment: {}",
                issues.join("; ")
            )));
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
        let error = environment.validate(1000.0).unwrap_err().to_string();
        assert!(error.contains("lights[0] (exhibit).power_w"));
    }
}
