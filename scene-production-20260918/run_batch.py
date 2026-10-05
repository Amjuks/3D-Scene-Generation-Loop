"""Reproduce the three reviewed scene categories, including export validation.

This fixed-category renderer does not interpret arbitrary natural-language prompts.
For that separate workflow, use Loop's scene task-list command.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess

ROOT=Path(__file__).resolve().parent
CATEGORIES=['japandi_reading','oak_dining','walnut_studio']
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--categories',nargs='+',choices=CATEGORIES,default=CATEGORIES)
p.add_argument('--output',type=Path,default=ROOT/'batches'/datetime.datetime.now().strftime('%Y%m%d-%H%M%S'))
p.add_argument('--blender',default=os.environ.get('BLENDER_PATH','/home/aman/3d_scene_generation/.cache/tools/blender-4.5.1-linux-x64/blender'))
p.add_argument('--samples',type=int,default=128)
p.add_argument('--width',type=int,default=2000)
p.add_argument('--resume',action='store_true')
args=p.parse_args()
output=args.output.resolve()
output.mkdir(parents=True,exist_ok=True)
env=os.environ.copy()
env.update(SHOWCASE_SAMPLES=str(args.samples),SHOWCASE_WIDTH=str(args.width))
results=[]
for name in args.categories:
    dest=output/name
    if dest.exists() and not args.resume:
        raise SystemExit(f'{dest} already exists; choose a new --output or use --resume')
    dest.mkdir(exist_ok=True)
    try:
        steps=[('build',ROOT/'build_scenes.py',[name,str(dest)],dest/'manifest.json'),
               ('validate',ROOT/'validate_exports.py',[str(dest)],dest/'export-validation.json'),
               ('detail',ROOT/'render_detail.py',[name,str(dest)],dest/'detail.png')]
        for step,script,params,evidence in steps:
            ready = evidence.exists()
            if step == 'build':
                ready = ready and all((dest/f).is_file() and (dest/f).stat().st_size > 1024
                                      for f in ['scene.blend','scene.glb','hero.png'])
            if step == 'validate':
                ready = ready and (dest/'glb-check.png').is_file()
            if args.resume and ready:
                if step=='validate' and not json.loads(evidence.read_text()).get('geometry_parity_passed'):
                    raise RuntimeError('Existing geometry validation failed')
                continue
            print(f'{name}: {step}',flush=True)
            with (dest/(step+'.log')).open('w') as log:
                subprocess.run([args.blender,'--background','--threads','24','--python-exit-code','1',
                                '--python',str(script),'--',*params],env=env,
                               stdout=log,stderr=subprocess.STDOUT,check=True)
            if not evidence.is_file():
                raise RuntimeError(f'{step} did not produce {evidence.name}')
        parity=json.loads((dest/'export-validation.json').read_text())
        if not parity['geometry_parity_passed']:
            raise RuntimeError('Export parity failed')
        results.append({'category':name,'completed':True,'directory':str(dest)})
    except Exception as error:
        results.append({'category':name,'completed':False,'error':str(error)})
(output/'batch-report.json').write_text(json.dumps(results,indent=2))
print(json.dumps(results,indent=2))
raise SystemExit(0 if all(r['completed'] for r in results) else 1)
