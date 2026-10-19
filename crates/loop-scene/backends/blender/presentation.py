"""Validated scene-authored illumination. Geometry and source models stay intact.

World coordinates are metres, +Z up; azimuth is measured from +X toward +Y.
The physical sky supplies diffuse atmosphere and a separate sun supplies direct
light, avoiding a double solar disc. glTF preserves punctual lights; its format
does not carry Blender's world shader or area emitters.
"""
import math
import re

import bpy
from mathutils import Vector


def number(value, lo, hi):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not lo <= value <= hi:
        raise ValueError(f'lighting number must be finite within {lo}..{hi}')
    return value


def vector(value, color=False):
    if not isinstance(value, (list, tuple)) or len(value) != 3:
        raise ValueError('lighting needs three coordinates')
    return [number(v, 0 if color else -1e12, 1 if color else 1e12) for v in value]


def validate(spec):
    if not isinstance(spec, dict) or set(spec) - {'purpose','sky','background_color','strength','exposure','sun','lights'}:
        raise ValueError('invalid environment fields')
    if not isinstance(spec.get('purpose'), str) or not spec['purpose'].strip() or spec.get('sky') not in {'constant','physical_sky'}:
        raise ValueError('environment needs purpose and supported sky')
    vector(spec['background_color'], color=True)
    number(spec['strength'], 0, 10)
    number(spec['exposure'], -16, 16)
    sun = spec.get('sun')
    if spec['sky'] == 'physical_sky' and sun is None:
        raise ValueError('physical sky requires sun direction')
    if sun is not None:
        if not isinstance(sun, dict) or set(sun) != {'elevation_degrees','azimuth_degrees','energy','color','angular_size_degrees'}:
            raise ValueError('invalid sun fields')
        number(sun['elevation_degrees'], 0, 90)
        number(sun['azimuth_degrees'], -360, 360)
        number(sun['energy'], 0, 100)
        number(sun['angular_size_degrees'], .01, 45)
        vector(sun['color'], color=True)
    lights = spec.get('lights', [])
    if not isinstance(lights, list) or len(lights) > 64:
        raise ValueError('environment permits at most 64 authored lights')
    seen = set()
    for light in lights:
        if not isinstance(light, dict) or set(light) - {'id','purpose','kind','position','target','color','power_w','size_m','spot_angle_degrees'}:
            raise ValueError('invalid light fields')
        name = light.get('id', '')
        if not isinstance(name, str) or not re.fullmatch('[A-Za-z0-9_-]{1,64}', name) or name in seen:
            raise ValueError('light IDs must be unique safe names')
        seen.add(name)
        if light.get('kind') not in {'point','area','spot'} or not isinstance(light.get('purpose'), str) or not light['purpose'].strip():
            raise ValueError('light requires supported kind and purpose')
        position, target = vector(light['position']), vector(light['target'])
        if light['kind'] != 'point' and all(abs(a-b)<.001 for a,b in zip(position,target)):
            raise ValueError('directed light target equals position')
        vector(light['color'], color=True)
        number(light['power_w'], 0, 100_000_000)
        number(light['size_m'], .001, 100_000)
        number(light.get('spot_angle_degrees',45), 1, 179)


def apply(scene, spec):
    validate(spec)  # Validate everything before altering scene state.
    for obj in list(scene.objects):
        if obj.get('loop_authored_light'):
            bpy.data.objects.remove(obj, do_unlink=True)
    world = bpy.data.worlds.new('Scene authored environment')
    scene.world = world
    world.use_nodes = True
    background = world.node_tree.nodes.get('Background')
    background.inputs['Color'].default_value = (*spec['background_color'], 1)
    background.inputs['Strength'].default_value = spec['strength']
    scene.view_settings.exposure = spec['exposure']
    sun = spec.get('sun')
    if spec['sky'] == 'physical_sky':
        sky = world.node_tree.nodes.new('ShaderNodeTexSky')
        sky.sky_type = 'NISHITA'
        sky.sun_disc = False
        sky.sun_elevation = math.radians(sun['elevation_degrees'])
        sky.sun_rotation = math.radians(sun['azimuth_degrees'])
        world.node_tree.links.new(sky.outputs['Color'], background.inputs['Color'])
    def light_object(name, kind, color, energy):
        data = bpy.data.lights.new('authored_' + name, kind)
        data.color, data.energy = color, energy
        obj = bpy.data.objects.new(data.name, data)
        obj['loop_authored_light'] = True
        scene.collection.objects.link(obj)
        return obj
    if sun is not None and sun['energy'] > 0:
        obj = light_object('sun', 'SUN', sun['color'], sun['energy'])
        elevation, azimuth = map(math.radians, (sun['elevation_degrees'], sun['azimuth_degrees']))
        to_sun = Vector((math.cos(elevation)*math.cos(azimuth), math.cos(elevation)*math.sin(azimuth), math.sin(elevation)))
        obj.rotation_euler = (-to_sun).to_track_quat('-Z','Y').to_euler()
        obj.data.angle = math.radians(sun['angular_size_degrees'])
    for light in spec.get('lights', []):
        obj = light_object(light['id'], light['kind'].upper(), light['color'], light['power_w'])
        obj.location = light['position']
        obj['purpose'] = light['purpose']
        if light['kind'] != 'point':
            obj.rotation_euler = (Vector(light['target'])-obj.location).to_track_quat('-Z','Y').to_euler()
        if light['kind'] == 'area':
            obj.data.shape = 'DISK'
            obj.data.size = light['size_m']
        else:
            obj.data.shadow_soft_size = light['size_m']/2
        if light['kind'] == 'spot':
            obj.data.spot_size = math.radians(light.get('spot_angle_degrees',45))
            obj.data.spot_blend = .25
    return {'mode':'scene_authored', 'environment':spec,
            'export_limits':['Blender world shaders and area lights are not represented in glTF; use scene.blend for authoritative offline lighting.']}
