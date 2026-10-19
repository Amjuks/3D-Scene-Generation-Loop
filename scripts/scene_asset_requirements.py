#!/usr/bin/env python3
"""Track scene asset gaps, search candidates, and explicitly verify selections.

No network access occurs unless `search --provider polyhaven|all` is requested.
Discovery never downloads model files or silently binds an approximate match.
An artist may register a suitable source from any domain with scene_assets.py.
"""
import argparse
import contextlib
import fcntl
import hashlib
import json
from pathlib import Path

import scene_assets as assets


def digest_json(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def requirement(item):
    if isinstance(item, str):
        item = {"name": item}
    if not isinstance(item, dict):
        raise ValueError("Asset requirements must be strings or objects")
    allowed = {"id", "name", "description", "category", "reason", "asset_type", "query", "must_terms", "identity_terms", "exclude_terms", "dimensions_m", "max_scale_ratio", "allowed_licenses", "quality", "surface_use"}
    if set(item) - allowed:
        raise ValueError("Unknown requirement fields: " + ", ".join(sorted(set(item) - allowed)))
    name = item.get("name", item.get("description", ""))
    if not isinstance(name, str) or not name.strip():
        raise ValueError("Each requirement needs a nonempty name or description")
    result = {"name": name, "category": item.get("category", ""), "reason": item.get("reason", ""),
        "asset_type": item.get("asset_type", "models"), "query": item.get("query", name),
        "must_terms": item.get("must_terms", []), "exclude_terms": item.get("exclude_terms", []),
        "identity_terms": item.get("identity_terms", []),
        "dimensions_m": item.get("dimensions_m"), "max_scale_ratio": item.get("max_scale_ratio", 1.25),
        "allowed_licenses": item.get("allowed_licenses", ["CC0-1.0", "CC-BY-4.0", "user-provided"]),
        "quality": item.get("quality", "production"), "surface_use": item.get("surface_use")}
    if result["asset_type"] not in {"models", "textures"} or result["quality"] not in {"production", "draft"}:
        raise ValueError("Unsupported requirement type/quality")
    for key in ["category", "reason", "query"]:
        if not isinstance(result[key], str):
            raise ValueError(f"{key} must be text")
    for key in ["must_terms", "exclude_terms", "identity_terms", "allowed_licenses"]:
        if not isinstance(result[key], list) or any(not isinstance(term, str) or not term.strip() for term in result[key]):
            raise ValueError(f"{key} must be a list of nonempty strings")
    if not result["allowed_licenses"]:
        raise ValueError("At least one allowed license is required")
    if result["surface_use"] is not None and (not isinstance(result["surface_use"], str) or not result["surface_use"].strip() or result["asset_type"] != "textures"):
        raise ValueError("surface_use must be a nonempty string on a texture requirement")
    # Validate dimensions/terms even when the inventory is empty.
    match_candidates(result, [])
    result["id"] = item.get("id", "asset-" + digest_json(result)[:16])
    if not isinstance(result["id"], str) or not result["id"].strip():
        raise ValueError("Requirement ID must be a nonempty string")
    result["requirement_sha256"] = digest_json(result)
    result.update(status="unresolved", candidates=[], selection=None, findings=[])
    return result


def match_candidates(req, inventory, limit=12, local_quality=False):
    return assets.search_candidates(req["query"], inventory, asset_type=req["asset_type"],
        must=req["must_terms"], exclude=req["exclude_terms"], identity=req.get("identity_terms", []), licenses=req["allowed_licenses"],
        quality=req["quality"] if local_quality else None, dimensions_m=req["dimensions_m"],
        max_scale_ratio=req["max_scale_ratio"], surface_use=req.get("surface_use"), limit=limit)


def extract_requirements(document):
    if isinstance(document, list):
        return document
    if not isinstance(document, dict):
        raise ValueError("Input must be a SceneProgram, RegionPlan, or requirements object/list")
    if "requirements" in document:
        if not isinstance(document["requirements"], list):
            raise ValueError("requirements must be an array")
        return document["requirements"]
    if "unresolved_assets" not in document:
        raise ValueError("Input contains no requirements or unresolved_assets")
    if not isinstance(document["unresolved_assets"], list):
        raise ValueError("unresolved_assets must be an array")
    return document["unresolved_assets"]


def make_manifest(document, source, previous=None):
    requirements = [requirement(item) for item in extract_requirements(document)]
    if len({req["id"] for req in requirements}) != len(requirements):
        raise ValueError("Duplicate requirement IDs; combine identical requirements or provide unique IDs")
    previous_by_hash = {req["requirement_sha256"]: req for req in (previous or {}).get("requirements", [])}
    for req in requirements:
        old = previous_by_hash.get(req["requirement_sha256"])
        if old:
            req.update({key: old[key] for key in ["status", "candidates", "selection", "findings"]})
    return {"version": 1, "source": str(source), "source_sha256": digest_json(document),
        "capabilities": assets.PROVIDER_CAPABILITIES,
        "notice": "Selections require actual geometry and visual review in the scene; metadata verification is not a production-quality certificate.",
        "requirements": requirements}


def read_manifest(path):
    manifest = json.loads(Path(path).read_text())
    if manifest.get("version") != 1 or not isinstance(manifest.get("requirements"), list):
        raise ValueError("Unsupported requirements manifest")
    return manifest


@contextlib.contextmanager
def locked_manifest(path):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(str(path) + ".lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield path


def save_manifest(path, manifest):
    assets.atomic_bytes(path, (json.dumps(manifest, indent=2) + "\n").encode())


def verify_source(req, source, catalog):
    if not match_candidates(req, [source], local_quality=True):
        raise ValueError("Selected source lacks required metadata, dimensions, license, quality or query evidence; refine the requirement or curate a suitable source")
    source_path = Path(source.get("path", ""))
    if not source_path.is_absolute():
        source_path = Path(catalog).resolve().parent / source_path
    if source_path.suffix.lower() not in {".glb", ".blend"}:
        raise ValueError("Selected source must be a self-contained GLB or packed Blender file")
    sha256 = hashlib.sha256()
    with open(source_path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            sha256.update(chunk)
    digest = sha256.hexdigest()
    if digest != source.get("sha256", "").lower():
        raise ValueError("Selected source checksum differs from its catalogue declaration")
    if not source.get("creator") or not source.get("source_url"):
        raise ValueError("Selected source lacks creator/source provenance")
    return {"asset_id": source["id"], "sha256": digest, "license": source["license"],
        "source_url": source["source_url"], "creator": source["creator"],
        "quality": source["quality"], "object_name": source.get("object_name"),
        "kind": assets.source_kind(source), "surface_uses": source.get("surface_uses", []), "material_family": source.get("material_family"),
        "catalog": str(Path(catalog).resolve())}


def verify_manifest(manifest, catalog):
    sources = {source["id"]: source for source in assets.local_inventory(catalog)}
    for req in manifest["requirements"]:
        selection = req.get("selection")
        if not selection:
            req["status"] = "candidates_need_review" if req.get("candidates") else "unresolved"
            continue
        try:
            if selection["asset_id"] not in sources:
                raise ValueError("Selected source ID is absent from the catalogue")
            evidence = verify_source(req, sources[selection["asset_id"]], catalog)
            if any(selection.get(key) != evidence[key] for key in ["sha256", "license", "source_url", "creator", "quality", "object_name", "kind", "surface_uses", "material_family"]):
                raise ValueError("Selected source identity/provenance changed; inspect and explicitly bind again")
            req.update(status="ready_for_scene_review", findings=[])
        except (OSError, ValueError) as error:
            req.update(status="blocked", findings=[str(error)])
    return manifest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    init = commands.add_parser("init", help="Persist asset requirements from a program/region or explicit requirements JSON")
    init.add_argument("--input", type=Path, required=True)
    init.add_argument("--output", type=Path, required=True)
    search = commands.add_parser("search", help="Store evidence-ranked candidates; never choose or download a source")
    search.add_argument("--manifest", type=Path, required=True)
    search.add_argument("--catalog", type=Path, default=Path("scene-assets/catalog.json"))
    search.add_argument("--provider", choices=["local", "polyhaven", "all"], default="local")
    search.add_argument("--cache", type=Path, default=Path("scene-assets/provider-index"))
    search.add_argument("--offline", action="store_true")
    search.add_argument("--refresh", action="store_true")
    search.add_argument("--limit", type=int, default=12)
    bind = commands.add_parser("bind", help="Explicitly select a reviewed local source and record verified provenance")
    bind.add_argument("requirement_id")
    bind.add_argument("asset_id")
    bind.add_argument("--manifest", type=Path, required=True)
    bind.add_argument("--catalog", type=Path, required=True)
    bind.add_argument("--reason", required=True, help="Why this exact source satisfies the subject, morphology and style")
    verify = commands.add_parser("verify", help="Recheck selections, source identity and constraints; fail while any gap remains")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--catalog", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        path = args.output if args.command == "init" else args.manifest
        with locked_manifest(path):
            if args.command == "init":
                old = read_manifest(path) if path.exists() else None
                manifest = make_manifest(json.loads(args.input.read_text()), args.input.resolve(), old)
            else:
                manifest = read_manifest(path)
            if args.command == "search":
                inventory = assets.local_inventory(args.catalog) if args.provider in {"local", "all"} else []
                if args.provider in {"polyhaven", "all"}:
                    for asset_type in sorted({req["asset_type"] for req in manifest["requirements"]}):
                        inventory += assets.provider_inventory(asset_type, args.cache, offline=args.offline, refresh=args.refresh)
                for req in manifest["requirements"]:
                    eligible = [source for source in inventory if not source["available"] or source.get("quality") == req["quality"]]
                    req["candidates"] = match_candidates(req, eligible, args.limit)
                    if not req.get("selection"):
                        req["status"] = "candidates_need_review" if req["candidates"] else "unresolved"
                        req["findings"] = [] if req["candidates"] else ["No eligible metadata match. Supply a suitable artist/scanned source or search another authorized library; do not substitute unrelated geometry."]
            elif args.command == "bind":
                if not args.reason.strip():
                    raise ValueError("A suitability explanation is required")
                req = next((req for req in manifest["requirements"] if req["id"] == args.requirement_id), None)
                source = next((source for source in assets.local_inventory(args.catalog) if source["id"] == args.asset_id), None)
                if req is None or source is None:
                    raise ValueError("Unknown requirement or catalogue source ID")
                req["selection"] = dict(verify_source(req, source, args.catalog), reason=args.reason)
                req.update(status="ready_for_scene_review", findings=[])
            elif args.command == "verify":
                verify_manifest(manifest, args.catalog)
            save_manifest(path, manifest)
        counts = {state: sum(req["status"] == state for req in manifest["requirements"]) for state in sorted({req["status"] for req in manifest["requirements"]})}
        print(json.dumps({"manifest": str(path), "requirements": len(manifest["requirements"]), "states": counts,
            "ready_for_scene_review": all(req["status"] == "ready_for_scene_review" for req in manifest["requirements"])}, indent=2))
        if args.command == "verify" and any(req["status"] != "ready_for_scene_review" for req in manifest["requirements"]):
            return 2
        return 0
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.exit(1, f"Asset requirements failed: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
