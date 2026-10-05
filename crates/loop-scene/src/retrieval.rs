//! Persistent local vector retrieval and content-addressed asset metadata.

use crate::config::EmbeddingConfig;
use crate::spec::{Bounds, Interface};
use crate::storage::{atomic_write, hash_file};
use crate::{Result, SceneError};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Exact reusable-asset metadata, stored separately from vector payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetMetadata {
    /// Content hash identity.
    pub content_hash: String,
    /// Human semantic description indexed for retrieval.
    pub description: String,
    /// Semantic category.
    pub category: String,
    /// Style tags.
    pub styles: Vec<String>,
    /// Material tags.
    pub materials: Vec<String>,
    /// Construction summary.
    pub construction_summary: String,
    /// Canonical units (`m`).
    pub units: String,
    /// Geometry-local measured bounds.
    pub bounds: Bounds,
    /// Explicit origin/pivot.
    pub origin: [f64; 3],
    /// Local front direction.
    pub front: [f64; 3],
    /// Local up direction.
    pub up: [f64; 3],
    /// Connector frames.
    pub interfaces: Vec<Interface>,
    /// Parameter adaptation ranges.
    pub parameter_ranges: BTreeMap<String, [f64; 2]>,
    /// Named supported adaptations.
    pub supported_adaptations: Vec<String>,
    /// Backend and pinned version.
    pub backend_version: String,
    /// Recipe and version.
    pub recipe_version: String,
    /// Native `.blend` path.
    pub native_path: Option<PathBuf>,
    /// Portable `.glb` path.
    pub glb_path: Option<PathBuf>,
    /// Texture dependency paths.
    pub texture_paths: Vec<PathBuf>,
    /// Validation evidence.
    pub validation_evidence: serde_json::Value,
    /// Source URL.
    pub source_url: Option<String>,
    /// Creator/attribution.
    pub creator: Option<String>,
    /// Established license.
    pub license: Option<String>,
    /// Acquisition date (ISO-8601).
    pub acquisition_date: Option<String>,
    /// Permitted use statement.
    pub permitted_use: Option<String>,
    /// Quality status (`accepted` or `quarantined`).
    pub quality_status: String,
    /// Semantic tags.
    pub tags: Vec<String>,
}

/// Hard compatibility requirements applied after semantic search.
pub struct AssetQuery<'a> {
    /// Natural-language retrieval query.
    pub text: &'a str,
    /// Required category.
    pub category: Option<&'a str>,
    /// Maximum bounds in meters, if constrained.
    pub max_size: Option<[f64; 3]>,
    /// Required interface types.
    pub interface_types: &'a [String],
    /// Required backend version prefix.
    pub backend: Option<&'a str>,
    /// Accepted licenses.
    pub allowed_licenses: &'a [String],
    /// Result count.
    pub limit: usize,
}

/// Retrieved compatible asset and cosine score.
pub struct Candidate {
    /// Exact metadata.
    pub metadata: AssetMetadata,
    /// Cosine similarity.
    pub score: f32,
}

/// Pluggable local embedding implementation.
pub trait Embedder: Send + Sync {
    /// Model ID/version.
    fn model_id(&self) -> &str;
    /// Vector dimension.
    fn dimensions(&self) -> usize;
    /// Embed semantic text.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// Deterministic local semantic-feature hashing. It expands a versioned small
/// ontology before hashing word and character n-gram features. This is an
/// offline baseline, not a claim of neural embedding quality.
pub struct SemanticHashEmbedder {
    model: String,
    dimensions: usize,
}
impl SemanticHashEmbedder {
    /// Construct the versioned baseline.
    pub fn new(model: String, dimensions: usize) -> Self {
        Self { model, dimensions }
    }
}
impl Embedder for SemanticHashEmbedder {
    fn model_id(&self) -> &str {
        &self.model
    }
    fn dimensions(&self) -> usize {
        self.dimensions
    }
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let normalized = text.to_lowercase();
        let mut tokens: Vec<String> = normalized
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let originals = tokens.clone();
        for t in originals {
            for s in semantic_expansions(&t) {
                tokens.push((*s).into());
            }
        }
        let mut v = vec![0f32; self.dimensions];
        for token in tokens {
            feature(&mut v, &format!("w:{token}"), 1.0);
            let chars: Vec<char> = format!("^{token}$").chars().collect();
            for n in 3..=4 {
                for g in chars.windows(n) {
                    feature(&mut v, &format!("c:{}", g.iter().collect::<String>()), 0.35);
                }
            }
        }
        normalize(&mut v);
        Ok(v)
    }
}

/// External local embedder command. It is pinned by path/model configuration,
/// receives UTF-8 text on stdin, and must return a JSON float array.
pub struct CommandEmbedder {
    model: String,
    dimensions: usize,
    command: PathBuf,
}
impl Embedder for CommandEmbedder {
    fn model_id(&self) -> &str {
        &self.model
    }
    fn dimensions(&self) -> usize {
        self.dimensions
    }
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut child = std::process::Command::new(&self.command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| SceneError::Backend(format!("embedding command: {e}")))?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .map_err(|e| SceneError::Backend(e.to_string()))?;
        let out = child
            .wait_with_output()
            .map_err(|e| SceneError::Backend(e.to_string()))?;
        if !out.status.success() {
            return Err(SceneError::Backend(format!(
                "embedding command failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let mut v: Vec<f32> = serde_json::from_slice(&out.stdout)
            .map_err(|e| SceneError::Backend(format!("embedding output: {e}")))?;
        if v.len() != self.dimensions {
            return Err(SceneError::Backend(format!(
                "embedding dimension {} != {}",
                v.len(),
                self.dimensions
            )));
        }
        normalize(&mut v);
        Ok(v)
    }
}

/// Build the configured local embedder.
pub fn build_embedder(config: &EmbeddingConfig) -> Result<Box<dyn Embedder>> {
    match config.backend.as_str() {
        "semantic_hash_v1" => Ok(Box::new(SemanticHashEmbedder::new(
            config.model.clone(),
            config.dimensions,
        ))),
        "command" => Ok(Box::new(CommandEmbedder {
            model: config.model.clone(),
            dimensions: config.dimensions,
            command: config.command.clone().unwrap(),
        })),
        x => Err(SceneError::Validation(format!(
            "unsupported embedding backend {x}"
        ))),
    }
}

/// SQLite exact-cosine index plus on-disk content store.
pub struct AssetLibrary {
    root: PathBuf,
    conn: Connection,
    embedder: Box<dyn Embedder>,
}
impl AssetLibrary {
    /// Open/create the local library and verify embedding identity.
    pub fn open(root: impl Into<PathBuf>, embedder: Box<dyn Embedder>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("objects")).map_err(store)?;
        std::fs::create_dir_all(root.join("quarantine")).map_err(store)?;
        let conn = Connection::open(root.join("index.db")).map_err(store)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS index_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE IF NOT EXISTS assets(content_hash TEXT PRIMARY KEY,metadata_json TEXT NOT NULL,index_text TEXT NOT NULL,quality_status TEXT NOT NULL);CREATE TABLE IF NOT EXISTS embeddings(content_hash TEXT PRIMARY KEY,model TEXT NOT NULL,dimensions INTEGER NOT NULL,vector BLOB NOT NULL,FOREIGN KEY(content_hash) REFERENCES assets(content_hash) ON DELETE CASCADE);").map_err(store)?;
        let old: Option<String> = conn
            .query_row(
                "SELECT value FROM index_meta WHERE key='embedding_identity'",
                [],
                |r| r.get(0),
            )
            .ok();
        let identity = format!("{}:{}", embedder.model_id(), embedder.dimensions());
        if old.as_deref().is_some_and(|v| v != identity) {
            return Err(SceneError::Storage(format!(
                "asset index uses {old:?}, configured {identity}; run index rebuild"
            )));
        }
        conn.execute(
            "INSERT OR REPLACE INTO index_meta(key,value) VALUES('embedding_identity',?1)",
            params![identity],
        )
        .map_err(store)?;
        Ok(Self {
            root,
            conn,
            embedder,
        })
    }
    /// Atomically promote and index a validated asset. Index failure does not
    /// delete the already-valid source artifact.
    pub fn add(
        &self,
        mut metadata: AssetMetadata,
        files: &[(&str, &Path)],
    ) -> Result<AssetMetadata> {
        if metadata.quality_status != "accepted" {
            return Err(SceneError::Validation(
                "only accepted assets enter normal index".into(),
            ));
        }
        let mut promoted = Vec::new();
        for (kind, path) in files {
            let hash = hash_file(path)?;
            if metadata.content_hash.is_empty() {
                metadata.content_hash = hash.clone();
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("bin");
            let dest = self
                .root
                .join("objects")
                .join(&hash[0..2])
                .join(format!("{hash}.{ext}"));
            if !dest.exists() {
                let bytes = std::fs::read(path).map_err(store)?;
                atomic_write(&dest, &bytes)?;
            }
            promoted.push(((*kind).to_string(), dest));
        }
        for (k, p) in promoted {
            match k.as_str() {
                "blend" => metadata.native_path = Some(p),
                "glb" => metadata.glb_path = Some(p),
                _ => metadata.texture_paths.push(p),
            }
        }
        let text = format!(
            "{} {} {} {} {}",
            metadata.description,
            metadata.category,
            metadata.styles.join(" "),
            metadata.materials.join(" "),
            metadata.construction_summary
        );
        let vector = self.embedder.embed(&text)?;
        let meta = serde_json::to_string(&metadata).map_err(store)?;
        let tx = self.conn.unchecked_transaction().map_err(store)?;
        tx.execute("INSERT OR REPLACE INTO assets(content_hash,metadata_json,index_text,quality_status) VALUES(?1,?2,?3,?4)",params![metadata.content_hash,meta,text,metadata.quality_status]).map_err(store)?;
        tx.execute("INSERT OR REPLACE INTO embeddings(content_hash,model,dimensions,vector) VALUES(?1,?2,?3,?4)",params![metadata.content_hash,self.embedder.model_id(),self.embedder.dimensions(),encode(&vector)]).map_err(store)?;
        tx.commit().map_err(store)?;
        Ok(metadata)
    }
    /// Semantic candidates followed by hard compatibility filters and ranking.
    pub fn search(&self, q: AssetQuery<'_>) -> Result<Vec<Candidate>> {
        let query = self.embedder.embed(q.text)?;
        let mut st=self.conn.prepare("SELECT a.metadata_json,e.vector FROM assets a JOIN embeddings e USING(content_hash) WHERE a.quality_status='accepted' AND e.model=?1 AND e.dimensions=?2").map_err(store)?;
        let rows = st
            .query_map(
                params![self.embedder.model_id(), self.embedder.dimensions()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .map_err(store)?;
        let allowed: HashSet<_> = q.allowed_licenses.iter().map(String::as_str).collect();
        let needed: HashSet<_> = q.interface_types.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        for row in rows {
            let (m, v) = row.map_err(store)?;
            let meta: AssetMetadata = serde_json::from_str(&m).map_err(store)?;
            if q.category.is_some_and(|c| meta.category != c)
                || q.backend
                    .is_some_and(|b| !meta.backend_version.starts_with(b))
            {
                continue;
            }
            if meta
                .license
                .as_deref()
                .is_some_and(|l| !allowed.contains(l))
                || meta.license.is_none() && !allowed.is_empty()
            {
                continue;
            }
            if let Some(max) = q.max_size {
                let size = meta.bounds.size();
                if (0..3).any(|i| size[i] > max[i] + 1e-6) {
                    continue;
                }
            }
            let have: HashSet<_> = meta
                .interfaces
                .iter()
                .map(|i| i.interface_type.as_str())
                .collect();
            if !needed.is_subset(&have) {
                continue;
            }
            out.push(Candidate {
                score: cosine(&query, &decode(&v, self.embedder.dimensions())?),
                metadata: meta,
            });
        }
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.metadata.content_hash.cmp(&b.metadata.content_hash))
        });
        out.truncate(q.limit);
        Ok(out)
    }
    /// Rebuild all embeddings from durable exact metadata.
    pub fn rebuild(&self) -> Result<usize> {
        let mut st = self
            .conn
            .prepare(
                "SELECT content_hash,metadata_json FROM assets WHERE quality_status='accepted'",
            )
            .map_err(store)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(store)?;
        let mut items = Vec::new();
        for r in rows {
            items.push(r.map_err(store)?);
        }
        let tx = self.conn.unchecked_transaction().map_err(store)?;
        tx.execute("DELETE FROM embeddings", []).map_err(store)?;
        for (hash, json) in &items {
            let m: AssetMetadata = serde_json::from_str(json).map_err(store)?;
            let text = format!(
                "{} {} {} {} {}",
                m.description,
                m.category,
                m.styles.join(" "),
                m.materials.join(" "),
                m.construction_summary
            );
            let v = self.embedder.embed(&text)?;
            tx.execute(
                "INSERT INTO embeddings(content_hash,model,dimensions,vector) VALUES(?1,?2,?3,?4)",
                params![
                    hash,
                    self.embedder.model_id(),
                    self.embedder.dimensions(),
                    encode(&v)
                ],
            )
            .map_err(store)?;
        }
        tx.commit().map_err(store)?;
        Ok(items.len())
    }
}

fn semantic_expansions(token: &str) -> &'static [&'static str] {
    match token {
        "sofa" | "couch" => &["seating", "furniture", "upholstered"],
        "chair" => &["seating", "furniture"],
        "table" | "desk" => &["surface", "furniture"],
        "oak" | "pine" | "timber" | "wood" => &["wooden", "natural", "carpentry"],
        "lamp" | "light" => &["lighting", "fixture", "illumination"],
        "cabinet" | "cupboard" => &["storage", "furniture", "joinery"],
        "stone" | "masonry" => &["mineral", "wall", "construction"],
        "room" | "interior" => &["space", "indoor"],
        "garden" | "plant" | "tree" => &["vegetation", "landscape", "outdoor"],
        _ => &[],
    }
}
fn feature(v: &mut [f32], s: &str, w: f32) {
    let h = Sha256::digest(s.as_bytes());
    let i = u64::from_le_bytes(h[0..8].try_into().unwrap()) as usize % v.len();
    let sign = if h[8] & 1 == 0 { 1.0 } else { -1.0 };
    v[i] += w * sign;
}
fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v {
            *x /= n
        }
    }
}
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn encode(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn decode(b: &[u8], d: usize) -> Result<Vec<f32>> {
    if b.len() != d * 4 {
        return Err(SceneError::Storage("corrupt embedding vector".into()));
    }
    Ok(b.chunks_exact(4)
        .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
        .collect())
}
fn store(e: impl std::fmt::Display) -> SceneError {
    SceneError::Storage(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_semantic_retrieval_filters_size() {
        let d = tempfile::tempdir().unwrap();
        let open = || {
            AssetLibrary::open(
                d.path(),
                Box::new(SemanticHashEmbedder::new("v1".into(), 128)),
            )
            .unwrap()
        };
        let f = d.path().join("x.glb");
        std::fs::write(&f, b"asset").unwrap();
        let lib = open();
        lib.add(
            AssetMetadata {
                content_hash: String::new(),
                description: "oak dining chair".into(),
                category: "chair".into(),
                styles: vec!["rustic".into()],
                materials: vec!["wood".into()],
                construction_summary: "joined timber seating".into(),
                units: "m".into(),
                bounds: Bounds {
                    min: [0.0; 3],
                    max: [0.5, 0.5, 1.0],
                },
                origin: [0.0; 3],
                front: [0.0, -1.0, 0.0],
                up: [0.0, 0.0, 1.0],
                interfaces: vec![],
                parameter_ranges: BTreeMap::new(),
                supported_adaptations: vec![],
                backend_version: "blender-4.5".into(),
                recipe_version: "chair-v1".into(),
                native_path: None,
                glb_path: None,
                texture_paths: vec![],
                validation_evidence: serde_json::json!({"ok":true}),
                source_url: None,
                creator: None,
                license: Some("CC0-1.0".into()),
                acquisition_date: None,
                permitted_use: Some("reuse".into()),
                quality_status: "accepted".into(),
                tags: vec![],
            },
            &[("glb", &f)],
        )
        .unwrap();
        drop(lib);
        let lib = open();
        let q = AssetQuery {
            text: "wooden seating",
            category: Some("chair"),
            max_size: Some([0.6, 0.6, 1.2]),
            interface_types: &[],
            backend: Some("blender"),
            allowed_licenses: &["CC0-1.0".into()],
            limit: 2,
        };
        assert_eq!(lib.search(q).unwrap().len(), 1);
    }
}
