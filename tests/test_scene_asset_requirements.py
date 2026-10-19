"""Evidence-based discovery and persistent asset-gap workflow, without network."""
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))
import scene_assets as assets
import scene_asset_requirements as requirements


def candidate(id="pine", **changes):
    return dict({"id": id, "name": "Pine tree", "category": "conifers", "tags": ["pine", "needles"],
        "provider": "local", "available": True, "asset_type": "models", "quality": "production",
        "license": "CC0-1.0", "dimensions_m": [1, 1, 2]}, **changes)


class AssetDiscoveryTests(unittest.TestCase):
    def test_token_boundaries_reject_substring_false_matches(self):
        results = assets.search_candidates("pine", [candidate(), candidate("food", name="Pineapple", category="fruit", tags=[])])
        self.assertEqual([row["id"] for row in results], ["pine"])
        self.assertEqual(results[0]["evidence"]["name"], ["pine"])
        self.assertTrue(results[0]["requires_suitability_review"])

    def test_hard_terms_exclusions_and_query_coverage_are_visible(self):
        sources = [candidate("young", tags=["young", "pine"]), candidate("mature", tags=["mature", "pine"])]
        result = assets.search_candidates("mature pine tree", sources, must=["pine"], exclude=["young"])
        self.assertEqual([row["id"] for row in result], ["mature"])
        self.assertEqual(result[0]["query_coverage"], 1)
        partial = assets.search_candidates("blue pine tree", sources)
        self.assertEqual(partial[0]["unmatched_terms"], ["blue"])
        self.assertEqual(assets.search_candidates("pine", sources, must=["oak"]), [])

    def test_species_identity_cannot_be_established_by_broad_provider_tags(self):
        sources = [candidate(), candidate("fir", name="Fir tree", category="conifer", tags=["pine", "woodland"])]
        result = assets.search_candidates("pine tree", sources, identity=["pine"])
        self.assertEqual([row["id"] for row in result], ["pine"])

    def test_physical_and_policy_constraints_do_not_pass_unknown_dimensions(self):
        sources = [candidate(), candidate("large", dimensions_m=[3, 3, 9]), candidate("unknown", dimensions_m=None), candidate("draft", quality="draft")]
        result = assets.search_candidates("pine", sources, dimensions_m=[1, 1, 2.1], max_scale_ratio=1.1, quality="production", licenses=["CC0-1.0"])
        self.assertEqual([row["id"] for row in result], ["pine"])
        for dimensions in [[0, 1, 2], [1, float("inf"), 2], [1, 2]]:
            with self.assertRaises(ValueError):
                assets.search_candidates("pine", [], dimensions_m=dimensions)

    def test_material_kind_and_usage_do_not_leak_into_models(self):
        surface = candidate("soil", name="Forest soil", tags=["pine"], asset_type="textures", surface_uses=["terrain"], material_family="soil")
        self.assertEqual(assets.search_candidates("pine", [surface]), [])
        self.assertEqual(assets.search_candidates("soil", [surface], asset_type="textures", surface_use="interior_wall"), [])
        result = assets.search_candidates("soil", [surface], asset_type="textures", surface_use="terrain")
        self.assertTrue(result[0]["usage_verified"])

    def test_current_provider_categories_attributes_and_offline_cache(self):
        metadata = {"wall": {"name": "Lime plaster", "category": "Building/Finishes/Plaster", "attributes": {"material": ["lime"], "condition": "worn"}, "description": "Fine textured lime plaster"}}
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(assets, "api", return_value=metadata) as api:
                source = assets.provider_inventory("textures", directory)
                api.assert_called_once_with("/assets?t=textures")
            with patch.object(assets, "api", side_effect=AssertionError("network")):
                self.assertEqual(source, assets.provider_inventory("textures", directory, offline=True, refresh=True))
                self.assertEqual(source, assets.provider_inventory("textures", directory))
            result = assets.search_candidates("lime plaster", source, asset_type="textures", must=["worn"])
            self.assertEqual(result[0]["id"], "polyhaven:wall")
            self.assertFalse(result[0]["available"])
            self.assertEqual(result[0]["quality"], "provider_source")

    def test_offline_missing_cache_and_ambiguous_ids_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                assets.provider_inventory("models", directory, offline=True)
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps({"version": 1, "assets": [{"id": "same"}, {"id": "same"}]}))
            with self.assertRaises(ValueError):
                assets.local_inventory(path)

    def test_local_kind_overrides_category_and_local_result_wins(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps({"version": 1, "assets": [{"id": "stone", "name": "Stone", "category": "stone", "kind": "material"}]}))
            inventory = assets.local_inventory(path)
            self.assertEqual(inventory[0]["asset_type"], "textures")
        results = assets.search_candidates("pine", [candidate("pine", provider="polyhaven", available=False), candidate()])
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["provider"], "local")


class AssetRequirementTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.catalog = self.root / "catalog.json"
        self.source = assets.register(self.catalog, {"id": "pine", "name": "Pine tree", "category": "conifer",
            "tags": ["young", "pine"], "license": "CC0-1.0", "creator": "Artist", "source_url": "local:artist",
            "quality": "production", "dimensions_m": [1, 1, 2]}, b"source-bytes", ".glb")
        self.input = self.root / "program.json"
        self.manifest = self.root / "requirements.json"
        self.input.write_text(json.dumps({"unresolved_assets": [{"name": "Young pine tree", "category": "conifer", "reason": "Need botanical detail"}]}))
        self.run_cli("init", "--input", self.input, "--output", self.manifest)

    def run_cli(self, *arguments):
        with contextlib.redirect_stdout(io.StringIO()):
            return requirements.main([str(arg) for arg in arguments])

    def load(self):
        return json.loads(self.manifest.read_text())

    def bind(self):
        return self.run_cli("bind", self.load()["requirements"][0]["id"], "pine", "--manifest", self.manifest,
            "--catalog", self.catalog, "--reason", "Inspected young pine morphology and source scale")

    def test_persistent_search_does_not_bind_or_download(self):
        with patch.object(assets, "api", side_effect=AssertionError("network")):
            self.run_cli("search", "--manifest", self.manifest, "--catalog", self.catalog)
        req = self.load()["requirements"][0]
        self.assertEqual(req["status"], "candidates_need_review")
        self.assertIsNone(req["selection"])
        self.assertEqual(req["candidates"][0]["id"], "pine")
        self.assertEqual(self.run_cli("verify", "--manifest", self.manifest, "--catalog", self.catalog), 2)

    def test_binding_persists_provenance_and_changed_bytes_block(self):
        self.bind()
        req = self.load()["requirements"][0]
        self.assertEqual(req["status"], "ready_for_scene_review")
        self.assertEqual(req["selection"]["sha256"], self.source["sha256"])
        self.assertEqual(self.run_cli("verify", "--manifest", self.manifest, "--catalog", self.catalog), 0)
        (self.root / self.source["path"]).write_bytes(b"tampered")
        self.assertEqual(self.run_cli("verify", "--manifest", self.manifest, "--catalog", self.catalog), 2)
        self.assertEqual(self.load()["requirements"][0]["status"], "blocked")
        self.assertIn("checksum", self.load()["requirements"][0]["findings"][0])

    def test_source_selection_metadata_changes_require_rebinding(self):
        self.bind()
        catalog = json.loads(self.catalog.read_text())
        catalog["assets"][0]["object_name"] = "another_tree"
        self.catalog.write_text(json.dumps(catalog))
        self.assertEqual(self.run_cli("verify", "--manifest", self.manifest, "--catalog", self.catalog), 2)
        self.assertIn("identity/provenance changed", self.load()["requirements"][0]["findings"][0])

    def test_unchanged_requirements_preserve_selection_changed_contract_resets(self):
        self.bind()
        self.run_cli("init", "--input", self.input, "--output", self.manifest)
        self.assertEqual(self.load()["requirements"][0]["selection"]["asset_id"], "pine")
        document = json.loads(self.input.read_text())
        document["unresolved_assets"][0]["must_terms"] = ["mature"]
        self.input.write_text(json.dumps(document))
        self.run_cli("init", "--input", self.input, "--output", self.manifest)
        self.assertIsNone(self.load()["requirements"][0]["selection"])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.bind()
        self.assertIsNone(self.load()["requirements"][0]["selection"])

    def test_unknown_constraints_cannot_be_silently_ignored(self):
        for item in [{"name": "Pine", "min_scale": 2}, {"name": "Pine", "must_terms": "pine"},
                     {"name": "Pine", "allowed_licenses": []}, {"name": "Pine", "surface_use": "terrain"}]:
            with self.assertRaises(ValueError):
                requirements.requirement(item)

    def test_no_candidates_keeps_explicit_gap(self):
        self.input.write_text(json.dumps({"requirements": [{"name": "Carved prayer altar", "must_terms": ["altar"]}]}))
        self.run_cli("init", "--input", self.input, "--output", self.manifest)
        self.run_cli("search", "--manifest", self.manifest, "--catalog", self.catalog)
        req = self.load()["requirements"][0]
        self.assertEqual(req["status"], "unresolved")
        self.assertIn("do not substitute", req["findings"][0])


if __name__ == "__main__":
    unittest.main()
