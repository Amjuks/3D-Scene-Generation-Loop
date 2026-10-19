import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('scene_diversity', Path(__file__).resolve().parents[1] / 'scripts/audit_scene_diversity.py')
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


def fixture():
    def node(name, recipe, position, parent='world'):
        return {'node_id': name, 'parent_id': parent, 'kind': 'furniture',
                'generation': {'strategy': 'recipe', 'recipe': recipe, 'parameters': {}, 'seed': 1},
                'bounds_local': {'min': [-.5, -.5, 0], 'max': [.5, .5, 1]},
                'transform': {'translation': position, 'rotation_xyzw': [0, 0, 0, 1], 'scale': [1, 1, 1]}}
    root = node('world', 'assembly', [0, 0, 0], None)
    root['generation']['strategy'] = 'assembly'
    return {'nodes': [root, node('chair-a', 'chair', [0, 0, 0]), node('chair-b', 'chair', [2, 0, 0]), node('table', 'table', [1, 2, 0])]}


class SceneDiversityTests(unittest.TestCase):
    def test_renaming_palette_and_world_transform_do_not_hide_repeated_layout(self):
        original, variant = fixture(), fixture()
        for index, node in enumerate(variant['nodes']):
            node['node_id'] = f'renamed-{index}'
            node['parent_id'] = 'renamed-0' if index else None
            node['purpose'] = 'a novel fantasy description'
            node['materials'] = [{'base_color': [1, 0, 1, 1]}]
            node['generation']['seed'] = 666
        variant['nodes'][0]['transform'].update(translation=[100, -20, 4], scale=[2, 2, 2])
        variant['nodes'].reverse()
        self.assertEqual(module.describe(original)['geometry_fingerprint'], module.describe(variant)['geometry_fingerprint'])

    def test_changed_layout_and_asset_content_have_distinct_fingerprints(self):
        original, variant = fixture(), fixture()
        variant['nodes'][1]['transform']['translation'] = [0, 3, 0]
        self.assertNotEqual(module.describe(original)['geometry_fingerprint'], module.describe(variant)['geometry_fingerprint'])
        original['nodes'][1]['generation'].update(recipe='asset', parameters={'asset_sha256': 'a' * 64})
        variant = copy.deepcopy(original)
        variant['nodes'][1]['generation']['parameters']['asset_sha256'] = 'b' * 64
        self.assertNotEqual(module.describe(original)['geometry_fingerprint'], module.describe(variant)['geometry_fingerprint'])

    def test_parent_rotation_is_composed_and_malformed_hierarchy_is_rejected(self):
        transformed = fixture()
        transformed['nodes'][0]['transform']['rotation_xyzw'] = [0, 0, 2**-.5, 2**-.5]
        self.assertAlmostEqual(module.describe(transformed)['extent_m'][0], 3)
        transformed['nodes'][0]['parent_id'] = 'chair-a'
        with self.assertRaisesRegex(ValueError, 'cyclic'):
            module.describe(transformed)
        transformed = fixture()
        transformed['nodes'][1]['parent_id'] = 'missing'
        with self.assertRaisesRegex(ValueError, 'missing parent'):
            module.describe(transformed)

    def test_report_compares_saved_geometry_and_keeps_errors_explicit(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            paths = [root / f'{i}.json' for i in range(3)]
            for path in paths[:2]:
                path.write_text(json.dumps(fixture()))
            paths[-1].write_text('{broken')
            report = module.audit(paths, nearest=1)
            self.assertEqual(report['scene_count'], 2)
            self.assertEqual(len(report['errors']), 1)
            self.assertEqual(len(report['repeated_geometry_groups']), 1)
            self.assertEqual(report['scenes'][0]['nearest_scenes'][0]['score'], 1)
            self.assertNotIn('accepted', report)
            self.assertEqual(module.main([str(paths[0]), str(paths[1]), '--output', str(root / 'report.json')]), 0)


if __name__ == '__main__':
    unittest.main()
