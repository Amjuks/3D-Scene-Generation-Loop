//! Deterministic bounded placement of full-detail source instances.
use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Distribution {
    pub count: usize,
    /// Region-local XY boundary, independent of any room or terrain recipe.
    pub polygon: Vec<[f64; 2]>,
    #[serde(default)]
    pub exclusions: Vec<Vec<[f64; 2]>>,
    pub min_spacing_m: f64,
    #[serde(default)]
    pub seed: u64,
    #[serde(default = "yaw")]
    pub yaw_degrees: [f64; 2],
    #[serde(default = "scale")]
    pub uniform_scale: [f64; 2],
}
fn yaw() -> [f64; 2] {
    [0.0, 360.0]
}
fn scale() -> [f64; 2] {
    [1.0, 1.0]
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placement {
    pub xy: [f64; 2],
    pub yaw: f64,
    pub scale: f64,
}
/// Shared by all source prototypes in a region. A cell is at least as large
/// as every requested spacing, so only nine neighboring cells need inspection.
pub(crate) struct SpacingIndex {
    cell_size: f64,
    cells: BTreeMap<(i64, i64), Vec<([f64; 2], f64)>>,
}
impl SpacingIndex {
    pub(crate) fn new(max_spacing: f64) -> Result<Self> {
        if !max_spacing.is_finite() || max_spacing < 0.001 {
            return Err(error("shared spacing must be finite and at least 0.001m"));
        }
        Ok(Self {
            cell_size: max_spacing,
            cells: BTreeMap::new(),
        })
    }
    fn cell(&self, xy: [f64; 2]) -> (i64, i64) {
        (
            (xy[0] / self.cell_size).floor() as i64,
            (xy[1] / self.cell_size).floor() as i64,
        )
    }
    fn separated(&self, xy: [f64; 2], spacing: f64) -> bool {
        let cell = self.cell(xy);
        (-1..=1).all(|x| {
            (-1..=1).all(|y| {
                self.cells
                    .get(&(cell.0 + x, cell.1 + y))
                    .is_none_or(|points| {
                        points.iter().all(|(p, previous_spacing)| {
                            distance(*p, xy) >= spacing.max(*previous_spacing)
                        })
                    })
            })
        })
    }
    fn insert(&mut self, xy: [f64; 2], spacing: f64) {
        self.cells
            .entry(self.cell(xy))
            .or_default()
            .push((xy, spacing));
    }
}
fn error(message: &str) -> SceneError {
    SceneError::Validation(format!("asset distribution: {message}"))
}
fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn edge_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / (d[0] * d[0] + d[1] * d[1]))
        .clamp(0.0, 1.0);
    distance(p, [a[0] + t * d[0], a[1] + t * d[1]])
}
fn inside(p: [f64; 2], polygon: &[[f64; 2]]) -> bool {
    let mut result = false;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            result = !result;
        }
    }
    result
}
fn clear_edges(p: [f64; 2], polygon: &[[f64; 2]], radius: f64) -> bool {
    (0..polygon.len())
        .all(|i| edge_distance(p, polygon[i], polygon[(i + 1) % polygon.len()]) >= radius)
}
fn validate_polygon(polygon: &[[f64; 2]]) -> Result<()> {
    if !(3..=128).contains(&polygon.len())
        || polygon
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 1e8)
    {
        return Err(error(
            "polygon needs 3..128 finite XY vertices within supported metric range",
        ));
    }
    let n = polygon.len();
    let mut area = 0.0;
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        if distance(a, b) < 1e-8 {
            return Err(error("polygon has zero-length edges"));
        }
        area += a[0] * b[1] - a[1] * b[0];
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            let (c, d) = (polygon[j], polygon[(j + 1) % n]);
            let intersects =
                cross(a, b, c) * cross(a, b, d) < 0.0 && cross(c, d, a) * cross(c, d, b) < 0.0;
            if intersects
                || edge_distance(c, a, b) < 1e-8
                || edge_distance(d, a, b) < 1e-8
                || edge_distance(a, c, d) < 1e-8
                || edge_distance(b, c, d) < 1e-8
            {
                return Err(error("polygon self-intersects or has overlapping edges"));
            }
        }
    }
    if area.abs() < 1e-8 {
        return Err(error("polygon has no area"));
    }
    Ok(())
}
fn random(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    ((z >> 11) as f64) / (1_u64 << 53) as f64
}

#[cfg(test)]
fn placements(
    distribution: &Distribution,
    size: [f64; 3],
    scene_seed: u64,
) -> Result<Vec<Placement>> {
    let mut spacing = SpacingIndex::new(distribution.min_spacing_m)?;
    placements_with_spacing(distribution, size, scene_seed, &mut spacing)
}

/// On failure the caller must discard the region's partial placement index.
pub(crate) fn placements_with_spacing(
    distribution: &Distribution,
    size: [f64; 3],
    scene_seed: u64,
    spacing: &mut SpacingIndex,
) -> Result<Vec<Placement>> {
    let d = distribution;
    if d.count == 0
        || d.count > 16384
        || d.exclusions.len() > 32
        || !d.min_spacing_m.is_finite()
        || d.min_spacing_m < 0.001
        || d.min_spacing_m > spacing.cell_size
        || d.yaw_degrees
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 360.0)
        || d.yaw_degrees[0] > d.yaw_degrees[1]
        || d.uniform_scale
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0 || *v > 100.0)
        || d.uniform_scale[0] > d.uniform_scale[1]
        || size.iter().any(|v| !v.is_finite() || *v <= 0.0)
    {
        return Err(error("invalid count, spacing, scale, yaw or source bounds"));
    }
    validate_polygon(&d.polygon)?;
    for polygon in &d.exclusions {
        validate_polygon(polygon)?;
    }
    let lo: [f64; 2] =
        std::array::from_fn(|i| d.polygon.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min));
    let hi: [f64; 2] = std::array::from_fn(|i| {
        d.polygon
            .iter()
            .map(|p| p[i])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let mut state = d.seed ^ scene_seed;
    let mut result = Vec::with_capacity(d.count);
    // Finite rejection budget; never silently reduce counts or relax clearance.
    for _ in 0..d.count.saturating_mul(500).min(1_000_000) {
        let xy = [
            lo[0] + random(&mut state) * (hi[0] - lo[0]),
            lo[1] + random(&mut state) * (hi[1] - lo[1]),
        ];
        let scale =
            d.uniform_scale[0] + random(&mut state) * (d.uniform_scale[1] - d.uniform_scale[0]);
        let yaw = d.yaw_degrees[0] + random(&mut state) * (d.yaw_degrees[1] - d.yaw_degrees[0]);
        let radius = size[0].hypot(size[1]) * scale * 0.5;
        if !inside(xy, &d.polygon)
            || !clear_edges(xy, &d.polygon, radius)
            || d.exclusions
                .iter()
                .any(|polygon| inside(xy, polygon) || !clear_edges(xy, polygon, radius))
        {
            continue;
        }
        if !spacing.separated(xy, d.min_spacing_m) {
            continue;
        }
        spacing.insert(xy, d.min_spacing_m);
        result.push(Placement { xy, yaw, scale });
        if result.len() == d.count {
            return Ok(result);
        }
    }
    Err(error(&format!("only {} of {} instances fit the exact boundary, exclusions, source footprint and spacing. Revise the authored domain/density; no requested instance was silently omitted",result.len(),d.count)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Distribution {
        serde_json::from_value(serde_json::json!({"count":100,"polygon":[[-20,-20],[20,-20],[20,20],[-20,20]],"exclusions":[[[-2,-20],[2,-20],[2,20],[-2,20]]],"min_spacing_m":1.5,"seed":5})).unwrap()
    }
    #[test]
    fn source_instances_are_reproducible_spaced_and_clear_of_paths() {
        let d = fixture();
        let first = placements(&d, [1.0, 1.0, 2.0], 42).unwrap();
        assert_eq!(first, placements(&d, [1.0, 1.0, 2.0], 42).unwrap());
        assert_ne!(first, placements(&d, [1.0, 1.0, 2.0], 43).unwrap());
        for (i, p) in first.iter().enumerate() {
            assert!(p.xy[0].abs() >= 2.0 + 0.5_f64.sqrt());
            assert!(p.xy.iter().all(|v| v.abs() <= 20.0 - 0.5_f64.sqrt()));
            assert_eq!(p.scale, 1.0);
            for q in &first[..i] {
                assert!(distance(p.xy, q.xy) >= 1.5);
            }
        }
    }
    #[test]
    fn impossible_or_invalid_distributions_fail_without_fewer_objects() {
        let mut d = fixture();
        d.min_spacing_m = 100.0;
        assert!(placements(&d, [1.0, 1.0, 2.0], 42)
            .unwrap_err()
            .to_string()
            .contains("only 1 of 100"));
        d = fixture();
        d.polygon = vec![[0.0, 0.0], [10.0, 10.0], [0.0, 10.0], [10.0, 0.0]];
        assert!(placements(&d, [1.0, 1.0, 2.0], 42).is_err());
        d = fixture();
        d.uniform_scale = [1.0, f64::NAN];
        assert!(placements(&d, [1.0, 1.0, 2.0], 42).is_err());
    }
}
