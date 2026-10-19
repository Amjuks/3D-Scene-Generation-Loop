"""Host request validation; real mesh/material regressions run inside Blender."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('inspect_asset', Path(__file__).resolve().parents[1] / 'scripts/inspect_scene_asset.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class AssetInspectionTests(unittest.TestCase):
    def test_exact_catalog_selection_and_relative_path(self):
        with tempfile.TemporaryDirectory() as directory:
            catalog = Path(directory) / 'catalog.json'
            catalog.write_text(json.dumps({'version': 1, 'assets': [{'id': 'oak', 'path': 'models/oak.glb', 'sha256': 'a'*64, 'object_name': 'Crown'}]}))
            request = worker.source_request(catalog=catalog, asset_id='oak', max_triangles=5000)
            self.assertEqual(request['path'], str(Path(directory) / 'models/oak.glb'))
            self.assertEqual(request['source']['object_name'], 'Crown')
            self.assertEqual(request['max_triangles'], 5000)
            with self.assertRaisesRegex(ValueError, 'not present uniquely'):
                worker.source_request(catalog=catalog, asset_id='different')
            with self.assertRaisesRegex(ValueError, 'exact registered'):
                worker.source_request(catalog=catalog, asset_id='oak', object_name='Trunk')

    def test_request_modes_and_budgets(self):
        for arguments in ({}, {'catalog': 'catalog.json'}, {'asset_id': 'oak'}, {'source_path': 'x.glb', 'max_triangles': 0}, {'source_path': 'x.glb', 'max_texture_bytes': -1}, {'source_path': 'x.glb', 'max_source_bytes': True}):
            with self.assertRaises(ValueError):
                worker.source_request(**arguments)
        request = worker.source_request(source_path='oak.glb', allowed_licenses=['CC0-1.0'])
        self.assertEqual(request['allowed_licenses'], ['CC0-1.0'])


if __name__ == '__main__':
    unittest.main()
