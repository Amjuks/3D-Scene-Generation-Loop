#!/usr/bin/env python3
"""Inspect exact source bytes in isolated Blender and persist technical evidence.

Example:
  python3 scripts/inspect_scene_asset.py --catalog scene-assets/catalog.json \
    --asset-id polyhaven:pine_sapling_small_a --output scene-assets/qa/pine.json
This never modifies catalogue/source data or declares artistic production quality.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

WORKER = Path(__file__).resolve().parents[1] / 'crates/loop-scene/backends/blender/asset_quality.py'


def source_request(*, catalog=None, asset_id=None, source_path=None, object_name=None,
                   allowed_licenses=None, max_triangles=None, max_texture_bytes=None,
                   max_source_bytes=2_000_000_000):
    if (catalog is not None) != (asset_id is not None):
        raise ValueError('Both --catalog and --asset-id are required together')
    if (catalog is not None) == (source_path is not None):
        raise ValueError('Select exactly one catalogue asset or source path')
    if catalog is not None:
        catalog = Path(catalog).resolve()
        document = json.loads(catalog.read_text())
        if document.get('version') != 1 or not isinstance(document.get('assets'), list):
            raise ValueError('Unsupported catalogue schema')
        selected = [entry for entry in document['assets'] if entry.get('id') == asset_id]
        if len(selected) != 1:
            raise ValueError(f'Catalogue asset {asset_id!r} is not present uniquely')
        source = dict(selected[0])
        path = Path(source['path'])
        if not path.is_absolute():
            path = catalog.parent / path
        if object_name is not None:
            raise ValueError('Catalogue inspection uses the exact registered object selection')
    else:
        path = Path(source_path)
        source = {'object_name': object_name}
    for name, value in [('max_triangles', max_triangles), ('max_texture_bytes', max_texture_bytes),
                        ('max_source_bytes', max_source_bytes)]:
        if value is not None and (not isinstance(value, int) or isinstance(value, bool) or value <= 0):
            raise ValueError(f'{name} must be a positive integer')
    request = {'path': str(path.resolve()), 'source': source, 'max_source_bytes': max_source_bytes}
    for name, value in [('allowed_licenses', allowed_licenses), ('max_triangles', max_triangles),
                        ('max_texture_bytes', max_texture_bytes)]:
        if value is not None:
            request[name] = value
    return request


def inspect_asset(request, output, *, blender=None, timeout=180):
    """Host API. Return JSON report; subprocess failure/timeouts raise exceptions.

    Old output is never accepted as evidence of a new failed inspection.
    """
    binary = blender or os.environ.get('BLENDER_BINARY') or shutil.which('blender')
    if not binary:
        raise ValueError('Provide --blender, BLENDER_BINARY, or Blender on PATH')
    if timeout <= 0:
        raise ValueError('timeout must be positive')
    output = Path(output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='loop-asset-inspect-', dir=output.parent) as temporary:
        temporary = Path(temporary)
        request_path, report_path = temporary / 'request.json', temporary / 'report.json'
        request_path.write_text(json.dumps(request, allow_nan=False))
        completed = subprocess.run([str(binary), '--background', '--factory-startup', '--disable-autoexec',
                                    '--python-exit-code', '31', '--python', str(WORKER), '--',
                                    '--request', str(request_path), '--output', str(report_path)],
                                   capture_output=True, text=True, timeout=timeout, check=False)
        # Keep worker diagnostics even on a failed Blender process. They contain
        # source paths and import messages, not credentials or provider calls.
        output.with_suffix(output.suffix + '.log').write_text(completed.stdout + completed.stderr)
        if completed.returncode != 0:
            raise RuntimeError(f'Blender inspection failed with exit {completed.returncode}; see {output}.log')
        report = json.loads(report_path.read_text())
        if report.get('schema_version') != 1 or report.get('technical_status') not in {'invalid', 'valid', 'valid_with_warnings'}:
            raise RuntimeError('Blender produced an unsupported inspection report')
        report_path.replace(output)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--catalog', type=Path)
    parser.add_argument('--asset-id')
    parser.add_argument('--source', type=Path)
    parser.add_argument('--object-name')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--blender')
    parser.add_argument('--timeout', type=int, default=180)
    parser.add_argument('--allow-license', action='append', help='Optional explicit license policy; repeat to allow multiple')
    parser.add_argument('--max-triangles', type=int, help='Per-instance consumer budget, reported as suitability warning')
    parser.add_argument('--max-texture-bytes', type=int, help='Estimated decoded texture memory budget')
    parser.add_argument('--max-source-bytes', type=int, default=2_000_000_000)
    args = parser.parse_args()
    try:
        request = source_request(catalog=args.catalog, asset_id=args.asset_id, source_path=args.source,
                                 object_name=args.object_name, allowed_licenses=args.allow_license,
                                 max_triangles=args.max_triangles, max_texture_bytes=args.max_texture_bytes,
                                 max_source_bytes=args.max_source_bytes)
        report = inspect_asset(request, args.output, blender=args.blender, timeout=args.timeout)
    except (ValueError, OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        parser.exit(2, str(error) + '\n')
    print(json.dumps({'report': str(args.output), 'technical_status': report['technical_status'],
                      'errors': len(report['errors']), 'warnings': len(report['warnings']),
                      'summary': report.get('summary')}))
    return 1 if report['technical_status'] == 'invalid' else 0


if __name__ == '__main__':
    raise SystemExit(main())
