# Asset discovery and preparation

`scene_assets.py` searches registered sources and the finite Poly Haven library.
It ranks metadata evidence and reports unmatched terms; it does not certify
semantic or visual suitability. Artist/scanned assets for any domain can be
registered as self-contained GLB or packed Blender files. A missing appropriate
source remains a gap, rather than becoming a generic prop.

```bash
# One bounded metadata request per type; later searches can be fully offline.
python3 scripts/scene_assets.py index --type models
python3 scripts/scene_assets.py index --type textures
python3 scripts/scene_assets.py search 'pine tree' --provider all --offline \
  --catalog scene-assets/catalog.json --identity pine --exclude dead
python3 scripts/scene_assets.py search 'lime plaster' --type textures \
  --provider polyhaven --offline --must lime --must plaster
```

`--identity` requires a word or phrase in the source name, ID or category. This
helps distinguish species/type identity from broad provider tags. `--must` and
`--exclude` constrain all indexed metadata. Whole tokens prevent false matches
such as `pine` matching `pineapple`. Search also supports allowed licenses,
declared quality, measured dimensions and a maximum size ratio. Unknown measured
dimensions cannot pass a dimension constraint. `--surface-use` rejects a material
whose curator restrictions exclude that exact use. Unrestricted/unknown material
use remains explicitly unverified in the result.

Metadata is cached in `scene-assets/provider-index/` for 24 hours. `--offline`
requires an existing index, and `--refresh` deliberately refreshes it. Model
downloads remain explicit through `fetch`; no discovery command acquires models.
Both old category arrays and current category paths/attributes are indexed.
The supported API is documented by [Poly Haven](https://polyhaven.com/our-api).
Powered by Poly Haven.

## Persistent requirements

Use the actual `unresolved_assets` from a scene program or regional plan, or
author stricter requirements like `examples/adaptive/asset-requirements.json`:

```bash
python3 scripts/scene_asset_requirements.py init \
  --input examples/adaptive/asset-requirements.json \
  --output scene-assets/preparation/requirements.json
python3 scripts/scene_asset_requirements.py search \
  --manifest scene-assets/preparation/requirements.json \
  --catalog scene-assets/catalog.json --provider all --offline
```

The manifest preserves the exact request, source hash, constraints and evidence
for every candidate. Search leaves each gap `unresolved` or
`candidates_need_review`. It never selects an approximate match automatically.
`init` preserves existing selections only when the full requirement contract is
unchanged. Unknown constraint fields are rejected rather than silently ignored.

After inspecting a suitable registered asset, explicitly bind it with a rationale:

```bash
python3 scripts/scene_asset_requirements.py bind joinery-chair \
  polyhaven:chinese_armchair \
  --manifest scene-assets/preparation/requirements.json \
  --catalog scene-assets/catalog.json \
  --reason 'Inspected Chinese hardwood joinery and curved back; source fits the requested exhibition subject'
python3 scripts/scene_asset_requirements.py verify \
  --manifest scene-assets/preparation/requirements.json \
  --catalog scene-assets/catalog.json
```

Binding checks eligibility, measured-dimension constraints, source bytes and
provenance, then records `ready_for_scene_review`. Actual geometry and image
validation still decide acceptance. Verification returns exit code 2 while gaps
remain or a source changes. Changes to hash, source selection, license, quality,
creator or material restrictions invalidate a recorded selection.

A structured requirement accepts `id`, `name`, `category`, `reason`, `query`,
`asset_type` (`models` or `textures`), `identity_terms`, `must_terms`,
`exclude_terms`, `dimensions_m`, `max_scale_ratio`, `allowed_licenses`, `quality`,
and `surface_use`. Simple existing `{name, category, reason}` gaps remain valid.
Scene programs are not rewritten by this utility. After preparing the catalogue,
resume the controller so it can plan and validate against the new exact sources.

## Register domain-specific assets and surfaces

```bash
python3 scripts/scene_assets.py add /path/to/artist-model.glb \
  --catalog scene-assets/catalog.json --id artist:gothic-oak-altar-v1 \
  --name 'Gothic carved oak altar' --category altar --tags gothic oak carved \
  --kind model --license user-provided --source-url local:commissioned-library \
  --creator 'Asset artist' --quality production --dimensions-m 2.4 0.9 1.1

python3 scripts/scene_assets.py add /path/to/lime-plaster-swatch.blend \
  --catalog scene-assets/catalog.json --id artist:lime-plaster-v1 \
  --name 'Lime plaster' --category 'surface material, plaster' --kind material \
  --material-family lime_plaster --surface-use interior_wall \
  --license user-provided --source-url local:scanned-surfaces \
  --creator 'Surface artist' --quality production
```

`production` is a curator declaration, not something inferred from polygon count.
Only supply model bytes and licenses you are entitled to use. No external account,
paid marketplace, purchase or automatic generative-model service is assumed.

Declared material uses are exact strings. For example, a forest-floor scan limited
to `terrain` and `landscape_ground` must not be used as an exhibition wall. Material
family and uses describe real source suitability; the fetcher never invents them.
