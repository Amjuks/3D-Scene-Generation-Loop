"""Independent GLB load, portability checks, PNG decoding and checksums."""
import hashlib
import json
import struct
import sys
from pathlib import Path

import numpy as np
import trimesh
from PIL import Image

folder = Path(sys.argv[1]).resolve()
path = folder / 'scene.glb'
raw = path.read_bytes()
magic, version, length = struct.unpack_from('<III', raw)
assert magic == 0x46546C67 and version == 2 and length == len(raw)
json_length, json_kind = struct.unpack_from('<II', raw, 12)
assert json_kind == 0x4E4F534A
document = json.loads(raw[20:20+json_length])
assert all('uri' not in b for b in document.get('buffers', [])), 'external buffer'
assert all('bufferView' in im for im in document.get('images', [])), 'external texture'
scene = trimesh.load_scene(path, process=False)
assert len(scene.geometry) > 0
assert np.isfinite(scene.bounds).all()
for mesh in scene.geometry.values():
    assert np.isfinite(mesh.vertices).all()
    assert len(mesh.faces) > 0
images = {}
for name in ['hero.png','detail.png','glb-check.png']:
    with Image.open(folder/name) as im:
        im.load()
        assert min(im.size) >= 700
        images[name] = list(im.size)
native = json.loads((folder/'export-validation.json').read_text())
assert native['geometry_parity_passed']
# Convert the independent loader's Y-up bounds into canonical Z-up.
lo, hi = scene.bounds
canonical = np.array([[lo[0],-hi[2],lo[1]],[hi[0],-lo[2],hi[1]]])
expected = np.array([native['native']['bounds']['min'],native['native']['bounds']['max']])
residual = float(np.max(np.abs(canonical-expected)))
assert residual < .005, residual
files = {}
for name in ['scene.blend','scene.glb','hero.png','detail.png','glb-check.png']:
    p=folder/name
    files[name]={'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
report={'passed':True,'loader':'trimesh '+trimesh.__version__,
        'geometry_count':len(scene.geometry),'triangles':sum(len(m.faces) for m in scene.geometry.values()),
        'embedded_images':len(document.get('images',[])),
        'independent_bounds_residual_m':residual,'renders':images,'files':files}
(folder/'independent-validation.json').write_text(json.dumps(report,indent=2))
print(json.dumps({'scene':folder.name,**{k:v for k,v in report.items() if k not in {'files','renders'}}}))
