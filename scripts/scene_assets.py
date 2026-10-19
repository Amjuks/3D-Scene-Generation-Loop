#!/usr/bin/env python3
"""Curate real models for Loop: local imports and bounded Poly Haven downloads.

Uses the Python standard library. glTF buffers and textures are packed losslessly
into one GLB; materials, UVs, authored meshes and instancing are preserved.
Powered by Poly Haven: https://polyhaven.com (CC0 models).
"""
import argparse
import copy
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import struct
import tempfile
import time
import urllib.parse
import urllib.request

AGENT = "LoopScenePipeline/2.0 (Powered by Poly Haven)"
API = "https://api.polyhaven.com"
PROVIDER_CAPABILITIES = {
    "local": {"formats": ["glb", "blend"], "coverage": "Only registered artist/scanned sources; any domain can be supplied, but none is guaranteed.", "acquisition": "Explicit add or variant; no automatic purchases or synthesis."},
    "polyhaven": {"types": ["models", "textures"], "license": "CC0-1.0", "coverage": "Finite curated library. Search metadata is candidate evidence, not proof of species, era, function or visual quality.", "acquisition": "Explicit fetch of a selected ID with size/checksum checks.", "credit": "Powered by Poly Haven — https://polyhaven.com"},
}


def request_bytes(url, limit):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or parsed.hostname not in {"api.polyhaven.com", "dl.polyhaven.org", "dl.polyhaven.com"}:
        raise ValueError("Only HTTPS Poly Haven API/download hosts are allowed")
    request = urllib.request.Request(url, headers={"User-Agent": AGENT})
    with urllib.request.urlopen(request, timeout=90) as response:
        if urllib.parse.urlparse(response.url).hostname not in {"api.polyhaven.com", "dl.polyhaven.org", "dl.polyhaven.com"}:
            raise ValueError("Unexpected download redirect")
        if int(response.headers.get("Content-Length", "0")) > limit:
            raise ValueError("Download exceeds byte budget")
        data = response.read(limit + 1)
        if len(data) > limit:
            raise ValueError("Download exceeds byte budget")
        return data


def api(path):
    return json.loads(request_bytes(API + path, 20_000_000))


def metadata_strings(value):
    """Both older categories arrays and current category/attributes API schema."""
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        return [s for item in value.values() for s in metadata_strings(item)]
    if isinstance(value, list):
        return [s for item in value for s in metadata_strings(item)]
    return []


def tokens(value):
    return re.findall(r"[^\W_]+", str(value).casefold(), flags=re.UNICODE)


def metadata_fields(asset):
    return {
        "name": tokens(asset.get("name", "") + " " + asset.get("id", "")),
        "category": tokens(" ".join(metadata_strings(asset.get("category", [])) + metadata_strings(asset.get("categories", [])))),
        "tags": tokens(" ".join(metadata_strings(asset.get("tags", [])) + metadata_strings(asset.get("attributes", {})) + metadata_strings(asset.get("material_family")))),
        "description": tokens(asset.get("description", "")),
    }


def local_inventory(catalog):
    document = json.loads(Path(catalog).read_text())
    if document.get("version") != 1 or not isinstance(document.get("assets"), list):
        raise ValueError("Unsupported catalogue schema")
    entries, ids = [], set()
    for asset in document["assets"]:
        if not isinstance(asset, dict) or not isinstance(asset.get("id"), str) or asset["id"] in ids:
            raise ValueError("Invalid/duplicate asset ID in catalogue")
        ids.add(asset["id"])
        entry = dict(asset, provider="local", available=True)
        entry["asset_type"] = "textures" if source_kind(asset) == "material" else "models"
        entries.append(entry)
    return entries


def provider_inventory(asset_type, cache, *, refresh=False, offline=False, max_age=86400):
    """Cache metadata only; model bytes are never acquired by discovery."""
    cache = Path(cache) / ("polyhaven-" + asset_type + ".json")
    cached = None
    if cache.exists():
        if cache.stat().st_size > 20_000_000:
            raise ValueError("Provider metadata cache exceeds size limit")
        cached = json.loads(cache.read_text())
        if cached.get("version") != 1 or cached.get("provider") != "polyhaven" or cached.get("type") != asset_type or not isinstance(cached.get("assets"), dict):
            raise ValueError("Invalid provider metadata cache")
    if offline and cached is None:
        raise ValueError(f"No offline metadata index for {asset_type}; run index first")
    if not offline and (refresh or cached is None or time.time() - cached.get("retrieved_at", 0) > max_age):
        inventory = api("/assets?t=" + asset_type)
        if not isinstance(inventory, dict):
            raise ValueError("Unexpected provider inventory")
        cached = {"version": 1, "provider": "polyhaven", "type": asset_type, "retrieved_at": time.time(), "assets": inventory}
        atomic_bytes(cache, (json.dumps(cached, indent=2) + "\n").encode())
    result = []
    for key, value in cached["assets"].items():
        result.append(dict(value, id="polyhaven:" + key, provider="polyhaven", available=False,
            asset_type=asset_type, license="CC0-1.0", quality="provider_source", source_url="https://polyhaven.com/a/" + key))
    return result


def source_kind(asset):
    kind = asset.get("kind") or ("material" if asset.get("category", "").lower().startswith("surface material") else "model")
    if kind not in {"model", "material"}:
        raise ValueError("Source kind must be model or material")
    return kind


def search_candidates(query, inventory, *, asset_type="models", must=(), exclude=(),
                      identity=(), licenses=(), quality=None, dimensions_m=None, max_scale_ratio=1.25, surface_use=None, limit=12):
    """Transparent lexical/constraint ranking. Never authorizes a substitution.

    Exact token boundaries avoid pine/pineapple and car/carpet collisions. Hard
    requirements require metadata evidence; missing dimensions cannot pass.
    """
    query_words = set(tokens(query))
    if not query_words:
        raise ValueError("A nonempty search query is required")
    if limit <= 0 or not math.isfinite(max_scale_ratio) or max_scale_ratio < 1:
        raise ValueError("limit must be positive and max-scale-ratio must be finite and >= 1")
    if dimensions_m is not None and (len(dimensions_m) != 3 or any(not math.isfinite(v) or v <= 0 for v in dimensions_m)):
        raise ValueError("dimensions-m requires three finite positive numbers")
    phrases = lambda items: [tokens(term) for term in items]
    required, excluded, identities = phrases(must), phrases(exclude), phrases(identity)
    if any(not phrase for phrase in required + excluded + identities):
        raise ValueError("Empty must/exclude terms are invalid")
    results = []
    for asset in inventory:
        if asset.get("asset_type") != asset_type:
            continue
        fields = metadata_fields(asset)
        wordset = {word for words in fields.values() for word in words}
        has_phrase = lambda phrase: any(any(words[i:i + len(phrase)] == phrase for i in range(len(words) - len(phrase) + 1)) for words in fields.values())
        if not all(has_phrase(phrase) for phrase in required) or any(has_phrase(phrase) for phrase in excluded):
            continue
        if not all(any(any(fields[key][i:i + len(phrase)] == phrase for i in range(len(fields[key]) - len(phrase) + 1)) for key in ["name", "category"]) for phrase in identities):
            continue
        if licenses and asset.get("license") not in licenses:
            continue
        if quality and asset.get("quality") != quality:
            continue
        if surface_use and asset.get("surface_uses") and surface_use not in asset["surface_uses"]:
            continue
        source_dimensions = asset.get("dimensions_m")
        scale_ratios = None
        if dimensions_m is not None:
            if not isinstance(source_dimensions, list) or len(source_dimensions) != 3 or any(not isinstance(v, (float, int)) or not math.isfinite(v) or v <= 0 for v in source_dimensions):
                continue
            scale_ratios = [wanted / actual for wanted, actual in zip(dimensions_m, source_dimensions)]
            if any(max(scale, 1 / scale) > max_scale_ratio for scale in scale_ratios):
                continue
        matched = query_words & wordset
        if not matched:
            continue
        weighted = sum(weight * len(query_words & set(fields[name])) for name, weight in [("name", 5), ("category", 3), ("tags", 2), ("description", 1)])
        evidence = {key: sorted(query_words & set(words)) for key, words in fields.items() if query_words & set(words)}
        results.append({"id": asset["id"], "name": asset.get("name", asset["id"]), "provider": asset["provider"],
            "asset_type": asset_type, "available": asset.get("available", False),
            "score": weighted, "query_coverage": round(len(matched) / len(query_words), 4),
            "matched_terms": sorted(matched), "unmatched_terms": sorted(query_words - matched), "evidence": evidence,
            "license": asset.get("license"), "quality": asset.get("quality"), "source_url": asset.get("source_url"),
            "dimensions_m": source_dimensions, "scale_ratios": scale_ratios,
            "material_family": asset.get("material_family"), "surface_uses": asset.get("surface_uses", []),
            "usage_verified": bool(surface_use and surface_use in asset.get("surface_uses", [])), "requires_suitability_review": True})
    results.sort(key=lambda value: (-value["query_coverage"], -value["score"], not value["available"], value["id"]))
    # Prefer locally curated metadata when a provider entry has the same ID.
    local_ids = {entry["id"] for entry in results if entry["available"]}
    return [entry for entry in results if entry["available"] or entry["id"] not in local_ids][:limit]


def safe_relative(name):
    path = PurePosixPath(name)
    if not name or path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
        raise ValueError(f"Unsafe glTF resource path: {name}")
    return path.as_posix()


def pack_gltf(document, resources):
    """Pack external buffers/images without re-meshing or losing PBR materials."""
    document = copy.deepcopy(document)
    binary = bytearray()

    def append(data):
        binary.extend(b"\0" * (-len(binary) % 4))
        offset = len(binary)
        binary.extend(data)
        return offset

    offsets = []
    for buffer in document.get("buffers", []):
        uri = safe_relative(buffer.get("uri", ""))
        data = resources[uri]
        if len(data) != buffer["byteLength"]:
            raise ValueError(f"Incorrect glTF buffer length: {uri}")
        offsets.append(append(data))
    views = document.setdefault("bufferViews", [])
    for view in views:
        index = view.get("buffer", 0)
        if index < 0 or index >= len(offsets) or view.get("byteOffset", 0) < 0 or view["byteLength"] < 0:
            raise ValueError("Invalid glTF buffer view")
        if view.get("byteOffset", 0) + view["byteLength"] > document["buffers"][index]["byteLength"]:
            raise ValueError("glTF buffer view exceeds its buffer")
        view["byteOffset"] = view.get("byteOffset", 0) + offsets[index]
        view["buffer"] = 0
    for image in document.get("images", []):
        if "uri" not in image:
            continue
        uri = safe_relative(image.pop("uri"))
        data = resources[uri]
        extension = Path(uri).suffix.lower()
        mime = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".webp": "image/webp"}.get(extension)
        if mime is None:
            raise ValueError(f"Unsupported glTF image: {uri}")
        image["mimeType"] = mime
        image["bufferView"] = len(views)
        views.append({"buffer": 0, "byteOffset": append(data), "byteLength": len(data)})
    document["buffers"] = [{"byteLength": len(binary)}]
    encoded = json.dumps(document, separators=(",", ":"), ensure_ascii=False).encode()
    encoded += b" " * (-len(encoded) % 4)
    binary.extend(b"\0" * (-len(binary) % 4))
    length = 12 + 8 + len(encoded) + 8 + len(binary)
    return struct.pack("<4sII", b"glTF", 2, length) + struct.pack("<I4s", len(encoded), b"JSON") + encoded + struct.pack("<I4s", len(binary), b"BIN\0") + binary


def atomic_bytes(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        temp = Path(stream.name)
        try:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
            os.replace(temp, path)
        finally:
            temp.unlink(missing_ok=True)


def register(catalog, entry, data, extension):
    """Content-addressed blobs and an atomically updated process-locked index."""
    catalog = catalog.resolve()
    digest = hashlib.sha256(data).hexdigest()
    blob = catalog.parent / "models" / (digest + extension)
    atomic_bytes(blob, data)
    entry = dict(entry, sha256=digest, path=str(blob.relative_to(catalog.parent)))
    catalog.parent.mkdir(parents=True, exist_ok=True)
    with open(str(catalog) + ".lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        index = json.loads(catalog.read_text()) if catalog.exists() else {"version": 1, "assets": []}
        if index.get("version") != 1 or not isinstance(index.get("assets"), list):
            raise ValueError("Unsupported catalogue schema")
        existing = next((a for a in index["assets"] if a["id"] == entry["id"]), None)
        def normalized(value):
            # New optional source fields must not make old downloads non-idempotent.
            return dict(value, kind=source_kind(value), material_family=value.get("material_family"),
                surface_uses=value.get("surface_uses", []), object_name=value.get("object_name"))
        if existing and normalized(existing) != normalized(entry):
            raise ValueError(f"Asset ID {entry['id']} already exists with different content/metadata; use a versioned ID")
        if not existing:
            index["assets"].append(entry)
            index["assets"].sort(key=lambda a: a["id"])
            atomic_bytes(catalog, (json.dumps(index, indent=2) + "\n").encode())
    return entry


def fetch(args):
    if args.id and len(args.ids) != 1:
        raise ValueError('--id requires exactly one source asset')
    if args.type != 'textures' and (args.material_family or args.surface_use):
        raise ValueError('Material family/use restrictions apply to texture sources')
    models = api("/assets?t=" + args.type)
    for asset_id in args.ids:
        if asset_id not in models:
            raise ValueError(f"Unknown {args.type} asset {asset_id}; search first")
        info = models[asset_id]
        files = api("/files/" + urllib.parse.quote(asset_id, safe=""))
        model = files.get("gltf", {}).get(args.resolution, {}).get("gltf")
        if not model:
            raise ValueError(f"{asset_id} has no glTF at {args.resolution}; no format substitution made")
        inventory = {Path(urllib.parse.urlparse(model["url"]).path).name: model, **model.get("include", {})}
        expected = sum(int(v["size"]) for v in inventory.values())
        if expected > args.max_bytes:
            raise ValueError(f"{asset_id} needs {expected} bytes, above {args.max_bytes} budget")
        remaining = args.max_bytes
        resources = {}
        for relative, metadata in inventory.items():
            relative = safe_relative(relative)
            data = request_bytes(metadata["url"], min(remaining, int(metadata["size"])))
            remaining -= len(data)
            if len(data) != int(metadata["size"]) or hashlib.md5(data).hexdigest() != metadata["md5"]:
                raise ValueError(f"Provider checksum/length mismatch for {relative}")
            resources[relative] = data
        main = Path(urllib.parse.urlparse(model["url"]).path).name
        document = json.loads(resources.pop(main))
        packed = pack_gltf(document, resources)
        authors = info.get("authors", {})
        entry = register(args.catalog, {
            "id": args.id or "polyhaven:" + asset_id,
            "name": info.get("name", asset_id),
            "category": ("surface material, " if args.type == "textures" else "") + ", ".join(metadata_strings(info.get("category")) or metadata_strings(info.get("categories")) or [args.type]),
            "tags": metadata_strings(info.get("tags", [])) + metadata_strings(info.get("attributes", {})),
            "license": "CC0-1.0",
            "source_url": "https://polyhaven.com/a/" + asset_id,
            "creator": ", ".join(authors) if authors else "Poly Haven",
            "quality": args.quality,
            "kind": "material" if args.type == "textures" else "model",
            "material_family": args.material_family,
            "surface_uses": args.surface_use,
            "dimensions_m": None,
        }, packed, ".glb")
        print(json.dumps({"id": entry["id"], "bytes": len(packed), "path": entry["path"], "credit": "Powered by Poly Haven"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    search = commands.add_parser("search", help="Rank local/provider metadata with explicit matching evidence; downloads no models")
    search.add_argument("query")
    search.add_argument("--limit", type=int, default=12)
    search.add_argument("--type", choices=["models", "textures"], default="models")
    search.add_argument("--provider", choices=["local", "polyhaven", "all"], default="polyhaven")
    search.add_argument("--catalog", type=Path, default=Path("scene-assets/catalog.json"))
    search.add_argument("--cache", type=Path, default=Path("scene-assets/provider-index"))
    search.add_argument("--offline", action="store_true")
    search.add_argument("--refresh", action="store_true")
    search.add_argument("--must", action="append", default=[], help="Required metadata word/phrase; repeat for multiple requirements")
    search.add_argument("--identity", action="append", default=[], help="Required word/phrase in source name, ID or category; tags alone cannot establish identity")
    search.add_argument("--exclude", action="append", default=[], help="Disallowed metadata word/phrase")
    search.add_argument("--license", action="append", default=[])
    search.add_argument("--quality", choices=["production", "draft"])
    search.add_argument("--dimensions-m", nargs=3, type=float)
    search.add_argument("--max-scale-ratio", type=float, default=1.25)
    search.add_argument("--surface-use", help="Required curator-approved material use when the source declares restrictions")
    index = commands.add_parser("index", help="Refresh a bounded Poly Haven metadata cache for offline searches")
    index.add_argument("--type", choices=["models", "textures"], default="models")
    index.add_argument("--cache", type=Path, default=Path("scene-assets/provider-index"))
    get = commands.add_parser("fetch", help="Explicitly download CC0 models and all referenced PBR textures")
    get.add_argument("ids", nargs="+")
    get.add_argument("--type", choices=["models", "textures"], default="models")
    get.add_argument("--catalog", type=Path, required=True)
    get.add_argument("--resolution", choices=["1k", "2k", "4k", "8k", "16k"], default="4k")
    get.add_argument("--max-bytes", type=int, default=100_000_000, help="Aggregate byte budget per asset")
    get.add_argument("--quality", choices=["production", "draft"], default="production")
    get.add_argument("--id", help="Exact new catalogue ID for one selected source/resolution")
    get.add_argument("--material-family", help="Curator-declared family for a texture source")
    get.add_argument("--surface-use", action="append", default=[], help="Exact permitted surface usage for a texture source; repeat as needed")
    add = commands.add_parser("add", help="Register a self-contained artist/scanned GLB or packed .blend")
    add.add_argument("file", type=Path)
    add.add_argument("--catalog", type=Path, required=True)
    for key in ["id", "name", "category", "license", "source-url", "creator"]:
        add.add_argument("--" + key, required=True)
    add.add_argument("--quality", choices=["production", "draft"], default="draft")
    add.add_argument("--tags", nargs="*", default=[])
    add.add_argument("--dimensions-m", nargs=3, type=float)
    add.add_argument("--object-name", help="Exact object/subtree to use from a multi-variant file")
    add.add_argument("--kind", choices=["model", "material"], help="Defaults from category for older catalogues")
    add.add_argument("--material-family", help="Curator-supplied physical family, e.g. limestone or woven wool")
    add.add_argument("--surface-use", action="append", default=[], help="Exact permitted material usage; repeat for several uses")
    variant = commands.add_parser("variant", help="Register one inspected object/subtree from an existing source file")
    variant.add_argument("source_id")
    variant.add_argument("--catalog", type=Path, required=True)
    variant.add_argument("--id", required=True)
    variant.add_argument("--name", required=True)
    variant.add_argument("--object-name", required=True)
    variant.add_argument("--dimensions-m", nargs=3, type=float)
    args = parser.parse_args()
    try:
        if args.command == "search":
            inventory = local_inventory(args.catalog) if args.provider in {"local", "all"} else []
            if args.provider in {"polyhaven", "all"}:
                inventory += provider_inventory(args.type, args.cache, offline=args.offline, refresh=args.refresh)
            results = search_candidates(args.query, inventory, asset_type=args.type, must=args.must, exclude=args.exclude,
                identity=args.identity, licenses=args.license, quality=args.quality, dimensions_m=args.dimensions_m,
                max_scale_ratio=args.max_scale_ratio, surface_use=args.surface_use, limit=args.limit)
            print(json.dumps({"query": args.query, "candidates": results, "capabilities": PROVIDER_CAPABILITIES,
                "notice": "Candidates require semantic and visual review; no model was acquired or substituted."}, indent=2))
        elif args.command == "index":
            inventory = provider_inventory(args.type, args.cache, refresh=True)
            print(json.dumps({"provider": "polyhaven", "type": args.type, "entries": len(inventory), "cache": str(args.cache), "capabilities": PROVIDER_CAPABILITIES["polyhaven"]}, indent=2))
        elif args.command == "fetch":
            if args.max_bytes <= 0:
                raise ValueError("max-bytes must be positive")
            fetch(args)
        elif args.command == "variant":
            catalog = args.catalog.resolve()
            inventory = json.loads(catalog.read_text())
            entry = next((a for a in inventory["assets"] if a["id"] == args.source_id), None)
            if entry is None:
                raise ValueError(f"Unknown source ID {args.source_id}")
            source = catalog.parent / entry["path"]
            data = source.read_bytes()
            if hashlib.sha256(data).hexdigest() != entry["sha256"]:
                raise ValueError("Source checksum changed")
            entry = dict(entry, id=args.id, name=args.name, object_name=args.object_name, dimensions_m=args.dimensions_m)
            print(json.dumps(register(catalog, entry, data, source.suffix.lower()), indent=2))
        else:
            ext = args.file.suffix.lower()
            if ext not in {".glb", ".blend"}:
                raise ValueError("Use a self-contained .glb or packed .blend")
            metadata = {k: getattr(args, k) for k in ["id", "name", "category", "license", "source_url", "creator", "quality", "tags", "dimensions_m", "object_name", "material_family"]}
            metadata["kind"] = args.kind or source_kind(metadata)
            metadata["surface_uses"] = args.surface_use
            entry = register(args.catalog, metadata, args.file.read_bytes(), ext)
            print(json.dumps(entry, indent=2))
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"Asset preparation failed: {error}\n")


if __name__ == "__main__":
    main()
