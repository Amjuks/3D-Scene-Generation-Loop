"""Checks lossless resource packing and acquisition boundaries without network."""
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import contextlib
import io

spec = importlib.util.spec_from_file_location("scene_assets", Path(__file__).parents[1] / "scripts" / "scene_assets.py")
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class AssetPreparationTests(unittest.TestCase):
    def test_multiple_buffers_and_images_preserve_offsets_and_materials(self):
        document = {"asset": {"version": "2.0"}, "buffers": [
            {"uri": "first.bin", "byteLength": 3}, {"uri": "second.bin", "byteLength": 5}],
            "bufferViews": [{"buffer": 1, "byteOffset": 1, "byteLength": 3}],
            "images": [{"uri": "textures/color.png"}],
            "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}, "alphaMode": "MASK"}]}
        blob = assets.pack_gltf(document, {"first.bin": b"abc", "second.bin": b"12345", "textures/color.png": b"PNGDATA"})
        magic, version, length = struct.unpack_from("<4sII", blob)
        self.assertEqual((magic, version, length), (b"glTF", 2, len(blob)))
        json_len = struct.unpack_from("<I", blob, 12)[0]
        packed = json.loads(blob[20:20 + json_len])
        binary = blob[28 + json_len:]
        view = packed["bufferViews"][0]
        self.assertEqual(binary[view["byteOffset"]:view["byteOffset"] + view["byteLength"]], b"234")
        self.assertEqual(packed["materials"], document["materials"])
        self.assertEqual(packed["images"][0]["mimeType"], "image/png")
        self.assertIn("uri", document["images"][0])  # input unchanged

    def test_paths_and_buffers_cannot_escape_declared_inputs(self):
        for name in ["../secret", "/etc/passwd", "https://evil/x", "textures/../../x", "C:\\x"]:
            with self.assertRaises(ValueError):
                assets.safe_relative(name)
        with self.assertRaises(ValueError):
            assets.pack_gltf({"buffers": [{"uri": "a.bin", "byteLength": 20}]}, {"a.bin": b"x"})
        with self.assertRaises(ValueError):
            assets.pack_gltf({"buffers": [{"uri": "a.bin", "byteLength": 1}], "bufferViews": [{"buffer": 0, "byteLength": 2}]}, {"a.bin": b"x"})

    def test_registry_keeps_content_identity_and_refuses_silent_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            first = assets.register(path, {"id": "oak"}, b"original", ".glb")
            self.assertEqual(first, assets.register(path, {"id": "oak"}, b"original", ".glb"))
            with self.assertRaises(ValueError):
                assets.register(path, {"id": "oak"}, b"different", ".glb")
            self.assertEqual(json.loads(path.read_text())["assets"], [first])

    def test_variant_preserves_source_and_rejects_changed_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            catalog = Path(directory) / "catalog.json"
            first = assets.register(catalog, {"id": "trees", "license": "CC0-1.0",
                "quality": "production", "source_url": "https://example.test/trees"}, b"source", ".glb")
            args = ["scene_assets", "variant", "trees", "--catalog", str(catalog),
                "--id", "tree-a", "--name", "Single tree", "--object-name", "Tree_A",
                "--dimensions-m", "1", "2", "3"]
            with patch("sys.argv", args), contextlib.redirect_stdout(io.StringIO()):
                assets.main()
            variant = next(a for a in json.loads(catalog.read_text())["assets"] if a["id"] == "tree-a")
            self.assertEqual(variant["sha256"], first["sha256"])
            self.assertEqual(variant["license"], first["license"])
            self.assertEqual(variant["object_name"], "Tree_A")
            self.assertEqual(variant["dimensions_m"], [1, 2, 3])
            (catalog.parent / first["path"]).write_bytes(b"changed")
            with patch("sys.argv", args), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                assets.main()
            self.assertEqual(error.exception.code, 1)


if __name__ == "__main__":
    unittest.main()
