use loop_scene::backends::{BlenderBackend, SceneBackend};
use loop_scene::{spec::SceneSpec, SceneConfig};

/// Explicit because it starts real Blender processes and renders an image.
#[tokio::test]
#[ignore = "requires local Blender 4.5"]
async fn blender_component_export_reopen_and_glb_render() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = SceneConfig::load(&workspace.join("examples/scene.yaml")).unwrap();
    let backend = BlenderBackend::new(config).unwrap();
    let caps = backend.capabilities().await.unwrap();
    assert!(caps.version.starts_with("4.5"));
    let spec: SceneSpec =
        serde_json::from_str(include_str!("../examples/complete-scene.json")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = backend
        .generate_component(&spec.nodes[0], dir.path())
        .await
        .unwrap();
    let parity = backend
        .validate_exports(&output.blend, &output.glb, dir.path())
        .await
        .unwrap();
    assert!(parity.exists());
    assert!(dir
        .path()
        .join("export-validation/glb-inspection.png")
        .exists());
}

/// Exercises bottom-up scene assembly, camera planning and required formats.
#[tokio::test]
#[ignore = "requires local Blender 4.5 and renderer"]
async fn blender_assembly_multiview_and_axis_parity() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut config = SceneConfig::load(&workspace.join("examples/scene.yaml")).unwrap();
    config.quality.preview_resolution = [480, 270];
    config.quality.preview_samples = 4;
    let backend = BlenderBackend::new(config).unwrap();
    let spec: SceneSpec =
        serde_json::from_str(include_str!("../examples/complete-scene.json")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = backend.assemble(&spec, dir.path()).await.unwrap();
    assert!(output.previews.len() >= 3);
    let parity = backend
        .validate_exports(&output.blend, &output.glb, dir.path())
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(parity).unwrap()).unwrap();
    assert_eq!(value["passed"], true);
}
