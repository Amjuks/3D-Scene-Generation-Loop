"""Loop-directed interior production, repaired and art-directed locally.

Blender 4.5: --background --python build_scenes.py -- japandi_reading output_dir
Textures are generated locally, packed into Blend, and embedded in GLB.
"""
import bpy
import math
import os
import sys
import json
import random
from pathlib import Path
from mathutils import Vector
import numpy as np

args = sys.argv[sys.argv.index('--') + 1:]
NAME, OUT = args[0], Path(args[1]).resolve()
OUT.mkdir(parents=True, exist_ok=True)
random.seed(7301)
bpy.ops.wm.read_factory_settings(use_empty=True)
S = bpy.context.scene
S.unit_settings.system = 'METRIC'
S.render.engine = 'CYCLES'
S.cycles.device = 'CPU'
S.cycles.samples = int(os.environ.get('SHOWCASE_SAMPLES', 64))
S.cycles.use_denoising = True
S.cycles.adaptive_threshold = .035
S.cycles.max_bounces = 8
S.cycles.transparent_max_bounces = 8
S.render.threads_mode = 'FIXED'
S.render.threads = 24
S.render.resolution_x = int(os.environ.get('SHOWCASE_WIDTH', 1500))
S.render.resolution_y = int(S.render.resolution_x * .75)
S.render.resolution_percentage = 100
S.render.image_settings.file_format = 'PNG'
S.view_settings.view_transform = 'AgX'
S.view_settings.look = 'AgX - Medium High Contrast'
S.view_settings.exposure = .3


def image_map(name, values, color=True):
    h, w = values.shape[:2]
    im = bpy.data.images.new(name, width=w, height=h, alpha=True)
    im.colorspace_settings.name = 'sRGB' if color else 'Non-Color'
    if values.ndim == 2:
        values = np.repeat(values[:, :, None], 3, axis=2)
    rgba = np.ones((h, w, 4), dtype=np.float32)
    rgba[:, :, :3] = np.clip(values, 0, 1)
    im.pixels.foreach_set(rgba.ravel())
    im.pack()
    return im


def material(name, color, rough=.5, metallic=0, texture=None):
    m = bpy.data.materials.new(name)
    m.diffuse_color = (*color, 1)
    m.use_nodes = True
    p = m.node_tree.nodes.get('Principled BSDF')
    p.inputs['Base Color'].default_value = (*color, 1)
    p.inputs['Roughness'].default_value = rough
    p.inputs['Metallic'].default_value = metallic
    if texture:
        n = 512
        y, x = np.mgrid[0:n, 0:n] / n
        rng = np.random.default_rng(sum(map(ord, name)))
        noise = rng.random((n, n))
        if texture == 'wood':
            def elongated_noise(nx, ny):
                coarse = rng.random((ny,nx))
                xs = np.linspace(0,1,nx)
                ys = np.linspace(0,1,ny)
                rows = np.array([np.interp(np.linspace(0,1,n),xs,row) for row in coarse])
                return np.array([np.interp(np.linspace(0,1,n),ys,rows[:,i]) for i in range(n)]).T
            grain = .45*elongated_noise(48,5) + .40*elongated_noise(240,10) + .15*noise
            # Small-scale color variation, without a plastic flat surface.
            factor = .90 + .17*grain
            height = grain
            strength = .23
        elif texture == 'fabric':
            weave = np.sin(x*2*math.pi*180)*np.sin(y*2*math.pi*180)
            height = .5 + .15*weave + .12*(noise-.5)
            factor = .9 + .12*height
            strength = .35
            p.inputs['Sheen Weight'].default_value = .25
        else:
            height = .5 + .16*(noise-.5) + .025*np.sin(x*85)*np.sin(y*79)
            factor = .93 + .12*height
            strength = .12
        # Color textures use sRGB pixel values; convert supplied linear colors.
        srgb = np.where(np.array(color) <= .0031308, np.array(color)*12.92,
                        1.055*np.array(color)**(1/2.4)-.055)
        tex = m.node_tree.nodes.new('ShaderNodeTexImage')
        tex.image = image_map(name+'_albedo', factor[:, :, None]*srgb)
        m.node_tree.links.new(tex.outputs['Color'], p.inputs['Base Color'])
        dx, dy = np.gradient(height)
        norm = np.dstack((-dy*strength*8, -dx*strength*8, np.ones_like(height)))
        norm /= np.linalg.norm(norm, axis=2)[:, :, None]
        nt = m.node_tree.nodes.new('ShaderNodeTexImage')
        nt.image = image_map(name+'_normal', norm*.5+.5, False)
        nm = m.node_tree.nodes.new('ShaderNodeNormalMap')
        nm.inputs['Strength'].default_value = .45
        m.node_tree.links.new(nt.outputs['Color'], nm.inputs['Color'])
        m.node_tree.links.new(nm.outputs['Normal'], p.inputs['Normal'])
    return m


oak = material('Quarter sawn natural oak', (.34,.205,.095), .39, texture='wood')
walnut = material('Oiled American walnut', (.13,.059,.026), .36, texture='wood')
plaster = material('Warm limewashed plaster', (.72,.68,.59), .87, texture='plaster')
cream = material('Ivory woven linen', (.64,.60,.49), .82, texture='fabric')
sage = material('Sage boucle upholstery', (.18,.26,.19), .86, texture='fabric')
caramel = material('Cognac upholstery', (.27,.115,.042), .48, texture='fabric')
rugmat = material('Oatmeal handwoven wool', (.43,.38,.29), .98, texture='fabric')
black = material('Powder coated charcoal', (.022,.026,.027), .43)
brass = material('Brushed antique brass', (.45,.28,.095), .28, .78)
white = material('Glazed ivory stoneware', (.69,.66,.57), .28)
clay = material('Hand thrown terracotta', (.28,.10,.054), .66, texture='plaster')
green = material('Deep green leaf', (.038,.12,.037), .39)
soil = material('Potting soil', (.028,.021,.012), 1)
paper = material('Warm book paper', (.70,.66,.55), .89)
glass = material('Clear glass', (.94,.98,1), .025)
glass.node_tree.nodes.get('Principled BSDF').inputs['Transmission Weight'].default_value = 1


def finish(o, name, mat, bevel=0, smooth=False):
    o.name = name
    o['semantic_id'] = name
    if mat:
        o.data.materials.append(mat)
    if bevel:
        mod = o.modifiers.new('Soft manufactured edges', 'BEVEL')
        mod.width = bevel
        mod.segments = 3
    if smooth:
        for p in o.data.polygons:
            p.use_smooth = True
    if o.type == 'MESH' and not smooth:
        mod = o.modifiers.new('Weighted corner normals', 'WEIGHTED_NORMAL')
        mod.keep_sharp = True
    return o


def box(name, loc, dims, mat, bevel=.012, rot=0):
    bpy.ops.mesh.primitive_cube_add(size=1, location=loc)
    o = bpy.context.object
    o.dimensions = dims
    bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
    o.rotation_euler.z = rot
    return finish(o, name, mat, min(bevel, min(dims)*.45))


def cylinder(name, loc, radius, height, mat, r2=None):
    bpy.ops.mesh.primitive_cone_add(vertices=48, radius1=radius,
                                  radius2=radius if r2 is None else r2,
                                  depth=height, location=loc)
    return finish(bpy.context.object, name, mat, .003, True)


def sphere(name, loc, scale, mat):
    bpy.ops.mesh.primitive_uv_sphere_add(segments=32, ring_count=16, radius=1, location=loc)
    o = bpy.context.object
    o.scale = scale
    bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
    return finish(o, name, mat, 0, True)


def beam(name, a, b, radius, mat, r2=None):
    a, b = Vector(a), Vector(b)
    o = cylinder(name, (a+b)/2, radius, (b-a).length, mat, r2)
    o.rotation_euler = (b-a).to_track_quat('Z','Y').to_euler()
    return o


def tube(name, pts, radius, mat):
    c = bpy.data.curves.new(name, 'CURVE')
    c.dimensions = '3D'
    c.resolution_u = 12
    c.bevel_depth = radius
    c.bevel_resolution = 3
    s = c.splines.new('BEZIER')
    s.bezier_points.add(len(pts)-1)
    for p, co in zip(s.bezier_points, pts):
        p.co = co
        p.handle_left_type = p.handle_right_type = 'AUTO'
    o = bpy.data.objects.new(name, c)
    bpy.context.collection.objects.link(o)
    c.materials.append(mat)
    return o


def lathe(name, loc, profile, mat, segments=64):
    verts = [(r*math.cos(a*2*math.pi/segments)+loc[0],
              r*math.sin(a*2*math.pi/segments)+loc[1], z+loc[2])
             for r,z in profile for a in range(segments)]
    faces = []
    for j in range(len(profile)-1):
        for a in range(segments):
            b = (a+1)%segments
            faces.append((j*segments+a,j*segments+b,(j+1)*segments+b,(j+1)*segments+a))
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(verts, [], faces)
    mesh.update()
    uv = mesh.uv_layers.new(name='UVMap')
    for poly in mesh.polygons:
        for li in poly.loop_indices:
            vi = mesh.loops[li].vertex_index
            uv.data[li].uv = ((vi%segments)/segments,(vi//segments)/(len(profile)-1))
    o = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(o)
    return finish(o, name, mat, 0, True)


def vase(name, x,y,z, scale=.8, mat=white):
    return lathe(name, (x,y,z), [(r*scale,h*scale) for r,h in
        [(0,0),(.095,0),(.12,.025),(.145,.16),(.115,.25),(.055,.31),(.055,.35),
         (.044,.35),(.044,.31),(.10,.25),(.13,.16),(.10,.025),(0,.025)]], mat)


def cushion(name, loc, dims, mat, tilt=0):
    o = box(name, loc, dims, mat, min(dims)*.35)
    o.modifiers.get('Soft manufactured edges').segments = 8
    o.rotation_euler.x = tilt
    # Bevelled upholstery has a soft silhouette without inflated ball forms.
    return o


def group_transform(before, x,y,angle):
    parent = bpy.data.objects.new('Furniture assembly',None)
    bpy.context.collection.objects.link(parent)
    for o in set(bpy.context.scene.objects)-before-{parent}:
        if o.parent is None:
            o.parent = parent
    parent.location = (x,y,0)
    parent.rotation_euler.z = angle


def chair(name, x,y,angle=0, lounge=False, upholstery=sage):
    before = set(S.objects)
    w,d,z = (.80,.80,.38) if lounge else (.48,.48,.46)
    for i, xx in enumerate([-w*.43,w*.43]):
        for j, yy in enumerate([-d*.40,d*.40]):
            beam(name+f' tapered leg {i}{j}',(xx*1.12,yy*1.13,.02),(xx,yy,z),.028,oak,.037)
        beam(name+f' side rail {i}',(xx,-d*.43,z-.05),(xx,d*.43,z-.05),.028,oak)
        if lounge:
            beam(name+f' arm support {i}',(xx,-d*.29,z-.03),(xx,-d*.28,z+.22),.023,oak)
            tube(name+f' curved arm {i}',[(xx,-d*.45,z+.22),(xx,-d*.12,z+.27),
                 (xx,d*.36,z+.30),(xx,d*.44,z+.14)],.033,oak)
    cushion(name+' seat cushion',(0,0,z+.045),(w*.86,d*.95,.14 if lounge else .085),upholstery)
    beam(name+' back crossbar',(-w*.42,d*.39,z+.38),(w*.42,d*.39,z+.38),.026,oak)
    for xx in [-w*.38,w*.38]:
        beam(name+' back upright',(xx,d*.35,z-.05),(xx,d*.57,z+.50),.025,oak)
    cushion(name+' back cushion',(0,d*.43,z+.31),(w*.87,.105,.44 if lounge else .35),upholstery,math.radians(-13))
    group_transform(before,x,y,angle)


def rug(x,y,w,d):
    box('Thick woven rug',(x,y,.014),(w,d,.028),rugmat,.012)
    for side in [-1,1]:
        for j in range(int(w/.045)):
            xx=x-w/2+j*.045
            tube('Rug fringe',[(xx,y+side*d/2,.018),(xx+.008,y+side*(d/2+.06),.009)],.0035,cream)


def round_table(x,y,z=.47,r=.40,mat=oak):
    cylinder('Rounded side table top',(x,y,z-.025),r,.05,mat)
    for a in [0,2.094,4.189]:
        beam('Splayed side table leg',(x+math.cos(a)*r*.70,y+math.sin(a)*r*.70,.02),
             (x+math.cos(a)*r*.5,y+math.sin(a)*r*.5,z-.055),.025,mat,.035)


def point_light(name, loc, energy, color):
    data=bpy.data.lights.new(name,'POINT'); data.energy=energy; data.color=color; data.shadow_soft_size=.12
    o=bpy.data.objects.new(name,data); S.collection.objects.link(o); o.location=loc


def lamp(x,y, height=1.55, pendant=False, base_z=0):
    if not pendant:
        cylinder('Lamp weighted base',(x,y,base_z+.027),.18,.055,brass)
        cylinder('Lamp brass stem',(x,y,(height+base_z)*.5),.016,height-base_z,brass)
    else:
        beam('Pendant flex',(x,y,height+.28),(x,y,2.95),.006,black)
    lathe('Open linen shade',(x,y,height-.10),[(.32,0),(.23,.38),(.22,.38),(.31,0)],cream)
    cylinder('Shade mounting hub',(x,y,height+.275),.022,.025,brass)
    if not pendant:
        beam('Lamp shade support',(x,y,height-.03),(x,y,height+.28),.006,brass)
    for angle in [0,2.094,4.189]:
        beam('Shade support spoke',(x,y,height+.275),
             (x+.222*math.cos(angle),y+.222*math.sin(angle),height+.275),.003,brass)
    sphere('Warm opal bulb',(x,y,height+.04),(.055,.055,.065),white)
    point_light('Warm lamp light',(x,y,height+.02),35,(1,.70,.40))


bookcolors=[(.13,.22,.19),(.46,.26,.15),(.59,.53,.39),(.18,.21,.26),(.36,.11,.068),(.65,.62,.54)]
bookmats=[material('Book cloth '+str(i),c,.86) for i,c in enumerate(bookcolors)]
def book(x,y,z,w=.045,h=.24,mat=None):
    mat=mat or random.choice(bookmats)
    box('Book paper block',(x,y+.002,z+h/2),(w-.007,.18,h-.012),paper,.002)
    box('Book spine',(x,y-.091,z+h/2),(w,.009,h),mat,.003)
    for side in [-1,1]:
        box('Book cloth cover',(x+side*(w/2-.002),y,z+h/2),(.004,.193,h),mat,.001)
    for dz in [.035,h-.03]:
        box('Book embossed rule',(x,y-.097,z+dz),(w*.62,.0015,.002),brass,.0005)


def shelves(x,y,width=1.65,height=2.20):
    for xx in [x-width/2,x+width/2]:
        box('Bookcase oak upright',(xx,y,height/2+.06),(.035,.34,height),oak,.006)
    box('Bookcase shadow back',(x,y+.155,height/2+.06),(width,.02,height),walnut,.004)
    for level in range(6):
        z=.07+level*(height-.035)/5
        box('Bookcase shelf',(x,y,z),(width+.04,.35,.033),oak,.004)
        if level==5: continue
        start=x-width/2+.075
        for j in range(random.randint(9,15)):
            w=random.uniform(.025,.055)
            book(start+w/2,y-.05,z+.019,w,random.uniform(.18,.29))
            start+=w+.012
        if level%2==0:
            vase('Shelf stoneware',x+width*.30,y-.045,z+.019,.65,clay if level==2 else white)


def leaf(name, start, end, width):
    a,b=Vector(start),Vector(end); direction=b-a
    cross=direction.cross(Vector((0,0,1))).normalized()
    verts=[]
    for i in range(17):
        t=i/16; c=a+direction*t+Vector((0,0,.07*math.sin(math.pi*t)))
        for j in [-1,-.5,0,.5,1]:
            p=c+cross*(j*width*math.sin(math.pi*t))+Vector((0,0,-abs(j)*.015))
            verts.append(tuple(p))
    faces=[(i*5+j,i*5+j+1,(i+1)*5+j+1,(i+1)*5+j) for i in range(16) for j in range(4)]
    me=bpy.data.meshes.new(name);me.from_pydata(verts,[],faces);me.update()
    o=bpy.data.objects.new(name,me);S.collection.objects.link(o);finish(o,name,green,0,True)
    mod=o.modifiers.new('Leaf thickness','SOLIDIFY');mod.thickness=.001
    tube('Leaf midrib',[start,tuple((a+b)/2+Vector((0,0,.07))),end],.0015,green)


def plant(x,y,height=1.5):
    lathe('Clay planter',(x,y,0),[(.12,0),(.20,.03),(.23,.32),(.225,.36),(.20,.36),(.195,.05),(.12,.02)],clay)
    cylinder('Dark soil',(x,y,.32),.20,.02,soil)
    for k in range(5):
        angle=k*2.4; h=height*(.65+.35*k/4); tip=(x+.12*math.cos(angle),y+.12*math.sin(angle),h)
        tube('Plant branching stem',[(x,y,.31),(x+.03*math.cos(angle),y+.03*math.sin(angle),h*.65),tip],.008,green)
        for t in [.45,.62,.79,1.]:
            a=(x+.12*math.cos(angle)*t,y+.12*math.sin(angle)*t,.30+(h-.30)*t)
            theta=angle+t*6
            b=(a[0]+.34*math.cos(theta),a[1]+.34*math.sin(theta),a[2]+.04)
            leaf('Curved ficus leaf',a,b,.095)


def artwork(x,y,z,w=.85,h=1.10):
    box('Framed artwork backing',(x,y,z),(w,.035,h),walnut,.003)
    box('Art linen canvas',(x,y-.021,z),(w-.07,.006,h-.07),cream,.001)
    art1=material('Artwork muted rust',(.31,.13,.072),.9)
    art2=material('Artwork charcoal',(.075,.089,.073),.9)
    for dx,dz,sx,sz,ma in [(-.12,.10,.19,.31,art1),(.12,-.12,.21,.25,art2)]:
        sphere('Abstract painted form',(x+dx,y-.03,z+dz),(sx,.002,sz),ma)
    for dx in [-w/2,w/2]:box('Art frame stile',(x+dx,y-.02,z),(.025,.045,h+.025),oak,.003)
    for dz in [-h/2,h/2]:box('Art frame rail',(x,y-.02,z+dz),(w,.045,.025),oak,.003)


def shell():
    # Real individual boards and narrow seams, with staggered joints.
    floorwoods=[material('Oak floor board '+str(i),(.27+i*.012,.16+i*.008,.075+i*.004),.46,texture='wood') for i in range(5)]
    box('Floor substructure',(0,0,-.055),(5.7,4.9,.09),walnut,0)
    for row in range(28):
        x=-2.70+row*.20
        y=-2.45
        first=.48+(row%3)*.47
        lengths=[first,1.4,1.4,1.4,1.4]
        for length in lengths:
            length=min(length,2.45-y)
            if length<=0:break
            box('Individual oak floorboard',(x,y+length/2,-.012),(.198,length-.003,.025),random.choice(floorwoods),.001)
            y+=length
    box('Rear plaster wall',(0,2.43,1.5),(5.7,.16,3),plaster,.006)
    box('Plaster ceiling',(0,0,3.06),(5.7,4.9,.12),plaster,.004)
    # Left wall opening: y -1.30 to 1.25, z .60 to 2.65.
    box('Window wall south pier',(-2.82,-1.88,1.5),(.16,1.14,3),plaster,.004)
    box('Window wall north pier',(-2.82,1.88,1.5),(.16,1.14,3),plaster,.004)
    box('Window wall sill section',(-2.82,-.025,.29),(.16,2.55,.58),plaster,.004)
    box('Window wall lintel',(-2.82,-.025,2.825),(.16,2.55,.35),plaster,.004)
    # Right wall deliberately ends before the exterior camera line.
    box('Right return wall',(2.82,.9,1.5),(.16,3.06,3),plaster,.004)
    box('Rear skirting',(0,2.32,.07),(5.54,.025,.14),oak,.004)
    box('Window skirting',(-2.72,0,.07),(.03,4.7,.14),oak,.004)
    for yy in [-1.32,1.27]:box('Window vertical frame',(-2.78,yy,1.625),(.10,.055,2.13),oak,.006)
    for zz in [.59,1.62,2.67]:box('Window transom',(-2.78,-.025,zz),(.10,2.65,.055),oak,.006)
    box('Window mullion',(-2.78,-.025,1.63),(.10,.045,2.05),oak,.006)
    box('Deep oak window sill',(-2.70,-.025,.59),(.38,2.75,.055),oak,.009)
    box('Window glazing',(-2.825,-.025,1.625),(.008,2.53,2.01),glass,.001)
    beam('Curtain rail',(-2.58,-1.65,2.78),(-2.58,1.6,2.78),.014,brass)
    for cy in [-1.37,1.32]:
        verts=[];faces=[]
        for i in range(49):
            for j in range(13):
                t=i/48; z=.09+j/12*2.61
                verts.append((-2.53+.055*math.sin(t*math.pi*12),cy+(t-.5)*.64,z))
        for i in range(48):
            for j in range(12):faces.append((i*13+j,(i+1)*13+j,(i+1)*13+j+1,i*13+j+1))
        me=bpy.data.meshes.new('Linen folds');me.from_pydata(verts,[],faces);me.update()
        uv=me.uv_layers.new(name='UVMap')
        for p in me.polygons:
            for li in p.loop_indices:
                vi=me.loops[li].vertex_index;uv.data[li].uv=(vi//13/48,vi%13/12)
        o=bpy.data.objects.new('Hanging linen curtain',me);S.collection.objects.link(o);finish(o,o.name,cream,0,True)
        solid=o.modifiers.new('Linen thickness','SOLIDIFY');solid.thickness=.001


def sideboard(x=1.2,y=2.05):
    for xx in [-.72,.72]:
        for yy in [-.20,.20]:beam('Sideboard tapered leg',(x+xx,y+yy,.015),(x+xx,y+yy,.22),.025,oak,.035)
    box('Sideboard carcass',(x,y,.57),(1.7,.48,.72),walnut,.012)
    for i in range(4):
        xx=x-.63+i*.42
        box('Sideboard framed door',(xx,y-.25,.57),(.409,.025,.665),oak,.006)
        beam('Brass door pull',(xx+.12,y-.273,.51),(xx+.12,y-.273,.63),.006,brass)
    box('Sideboard top',(x,y,.95),(1.75,.53,.04),oak,.009)
    vase('Sideboard ceramic',x+.52,y,.972,1.0,clay)
    vase('Small sideboard ceramic',x+.25,y-.03,.972,.55,white)


def cup(x,y,z):
    lathe('Stoneware coffee cup',(x,y,z),[(0,0),(.034,0),(.040,.075),(.037,.08),(.032,.08),(.030,.01),(0,.01)],white)
    tube('Cup loop handle',[(x+.035,y,z+.063),(x+.065,y,z+.061),(x+.065,y,z+.026),(x+.035,y,z+.022)],.005,white)


def build_reading():
    rug(-.25,-.15,2.55,2.1)
    chair('Sculpted lounge chair',-.65,.35,math.radians(-15),True,sage)
    for xx in [-.22,.22]:
        for yy in [-.17,.17]:beam('Ottoman tapered leg',(-.5+xx,-.80+yy,.03),(-.5+xx,-.80+yy,.30),.025,oak)
    cushion('Ottoman upholstered cushion',(-.5,-.8,.35),(.68,.52,.16),sage)
    round_table(.55,.22,.47,.40)
    cup(.60,.18,.471)
    # Closed magazine lies flat on the side table.
    box('Art journal',(.45,.22,.480),(.21,.16,.018),bookmats[1],.002)
    lamp(-1.67,.76,1.48)
    shelves(-1.70,2.10,1.50,2.25)
    sideboard(1.15,2.02)
    artwork(.95,2.325,1.93,1.20,1.25)
    plant(2.1,.90,1.75)


def build_dining():
    rug(0,0,3.45,2.55)
    for xx in [-.90,.90]:
        for yy in [-.38,.38]:box('Dining tapered square leg',(xx,yy,.36),(.075,.075,.72),oak,.012)
    for yy in [-.4,.4]:box('Long table apron',(0,yy,.645),(1.93,.032,.14),oak,.005)
    for xx in [-.94,.94]:box('Table end apron',(xx,0,.645),(.032,.8,.14),oak,.005)
    for j in range(5):box('Oak dining top plank',(0,(j-2)*.21,.755),(2.25,.208,.055),oak,.009)
    box('Linen runner',(0,0,.786),(1.94,.29,.005),cream,.002)
    for i,x in enumerate([-.67,.67]):
        chair('South dining chair '+str(i),x,-.92,math.pi,False,cream)
        chair('North dining chair '+str(i),x,.92,0,False,cream)
    chair('West dining chair',-1.48,0,math.pi/2,False,cream)
    chair('East dining chair',1.48,0,-math.pi/2,False,cream)
    for x in [-.65,.65]:
        for y in [-.34,.34]:
            lathe('Dinner plate',(x,y,.785),[(0,0),(.10,0),(.14,.012),(.145,.025),(.135,.030),(.09,.014),(0,.014)],white)
            cylinder('Water glass',(x+.20,y,.839),.030,.102,glass)
            box('Folded linen napkin',(x,y,.811),(.14,.09,.008),cream,.004)
            beam('Brass cutlery',(x-.18,y-.07,.79),(x-.18,y+.07,.79),.004,brass)
    vase('Table centerpiece',0,0,.79,.70,clay)
    for k in range(5):
        tip=(math.cos(k*2.4)*.22,math.sin(k*2.4)*.12,1.35+k*.02)
        tube('Vase branch',[(0,0,.96),(.02,0,1.16),tip],.003,walnut)
        leaf('Olive leaf',tip,(tip[0]+.09,tip[1]+.03,tip[2]+.02),.018)
    lamp(-.55,0,1.98,True);lamp(.55,0,1.98,True)
    sideboard(0,2.05);artwork(0,2.325,1.97,1.4,1.2)
    plant(2.1,1.65,1.85)


def build_studio():
    rug(0,-.25,3.2,2.4)
    for xx in [-.9,.9]:
        for yy in [-.30,.30]:beam('Desk metal support',(xx,yy,.02),(xx,yy,.74),.022,black)
        beam('Desk sled foot',(xx,-.36,.025),(xx,.36,.025),.022,black)
    box('Solid walnut desktop',(0,0,.765),(2.1,.85,.055),walnut,.018)
    box('Desk suspended drawer',( -.66,.02,.66),(.62,.63,.15),walnut,.009)
    beam('Drawer brass pull',(-.79,-.31,.66),(-.55,-.31,.66),.006,brass)
    chair('Desk chair',0,.88,0,True,caramel)
    # Monitor faces its user toward +Y.
    box('Monitor foot',(0,-.14,.805),(.30,.20,.018),black,.012)
    beam('Monitor neck',(0,-.18,.81),(0,-.21,1.00),.026,black)
    box('Thin monitor bezel',(0,-.22,1.18),(.74,.045,.44),black,.012)
    screen=material('Subtle blue display',(.035,.07,.09),.26)
    p=screen.node_tree.nodes.get('Principled BSDF');p.inputs['Emission Color'].default_value=(.025,.05,.075,1);p.inputs['Emission Strength'].default_value=.5
    box('Monitor screen',(0,-.194,1.18),(.695,.004,.394),screen,.003)
    box('Keyboard chassis',(0,.19,.81),(.46,.16,.02),black,.005)
    for row in range(5):
        for col in range(15):box('Individual keyboard key',(-.21+col*.029,.128+row*.026,.827),(.025,.022,.009),paper,.002)
    sphere('Ergonomic mouse',(.37,.20,.828),(.035,.057,.022),black)
    box('Desk notebook',(-.60,.14,.81),(.25,.19,.025),bookmats[0],.004)
    beam('Brass pen',(-.64,.08,.827),(-.53,.20,.827),.003,brass)
    cup(.67,.22,.80)
    lamp(-.76,-.18,1.17,base_z=.793)
    chair('Guest chair',.20,-1.0,math.pi,False,sage)
    shelves(-1.70,2.10,1.48,2.25)
    sideboard(1.12,2.04);artwork(1.05,2.325,1.95,1.15,1.22)
    plant(2.13,1.08,1.8)


shell()
{'japandi_reading':build_reading,'oak_dining':build_dining,'walnut_studio':build_studio}[NAME]()

def area(name, loc, target, power, size, color):
    d=bpy.data.lights.new(name,'AREA');d.energy=power;d.shape='DISK';d.size=size;d.color=color
    o=bpy.data.objects.new(name,d);S.collection.objects.link(o);o.location=loc
    o.visible_glossy=False
    o.visible_camera=False
    o.visible_transmission=False
    o.rotation_euler=(Vector(target)-o.location).to_track_quat('-Z','Y').to_euler()

S.world=bpy.data.worlds.new('Soft daylight environment');S.world.use_nodes=True
S.world.node_tree.nodes['Background'].inputs['Color'].default_value=(.65,.75,1,1)
S.world.node_tree.nodes['Background'].inputs['Strength'].default_value=.25
area('Large window daylight',(-4.5,-.3,3.4),(0,.7,.9),800,3.2,(1,.88,.72))
area('Photographic front bounce',(1,-4.0,3.5),(0,.6,.9),240,4.0,(.80,.88,1))
d=bpy.data.lights.new('Late afternoon sun','SUN');d.energy=1.3;d.angle=.12;d.color=(1,.86,.68)
sun=bpy.data.objects.new('Late afternoon sun',d);S.collection.objects.link(sun)
sun.rotation_euler=(math.radians(32),math.radians(-40),math.radians(-60))
camdata=bpy.data.cameras.new('Interior architectural camera');cam=bpy.data.objects.new('Interior architectural camera',camdata);S.collection.objects.link(cam)
cam.location=(2.60,-3.30,1.80)
target=Vector((-.20,.75,1.15))
cam.rotation_euler=(target-cam.location).to_track_quat('-Z','Y').to_euler()
camdata.lens=27;camdata.clip_start=.05;S.camera=cam
S.render.filepath=str(OUT/'hero.png')
# Curves become portable mesh geometry before export; native remains editable.
bpy.ops.wm.save_as_mainfile(filepath=str(OUT/'scene.blend'))
bpy.ops.render.render(write_still=True)
for o in list(S.objects):
    if o.type=='CURVE':
        bpy.ops.object.select_all(action='DESELECT');o.select_set(True);bpy.context.view_layer.objects.active=o
        bpy.ops.object.convert(target='MESH')
bpy.ops.object.select_all(action='DESELECT')
bpy.ops.export_scene.gltf(filepath=str(OUT/'scene.glb'),export_format='GLB',export_apply=True,
                          export_yup=True,export_extras=True,export_cameras=True,export_lights=True)
manifest={'name':NAME,'provenance':'Loop design direction plus supervised code repair and art direction',
 'seed':7301,'objects':len(S.objects),'mesh_objects':sum(o.type=='MESH' for o in S.objects),
 'materials':len(bpy.data.materials),'packed_texture_images':len(bpy.data.images),
 'camera':{'position':list(cam.location),'target':list(target),'lens_mm':camdata.lens},
 'render':{'engine':'Cycles CPU','samples':S.cycles.samples,'denoised':True,
           'resolution':[S.render.resolution_x,S.render.resolution_y]},
 'exports':['scene.blend','scene.glb','hero.png'],
 'glb_limits':'Embedded albedo and tangent normal textures; area lights and environment require viewer lighting. Geometry and textures are retained.'}
(OUT/'manifest.json').write_text(json.dumps(manifest,indent=2))
print('PRODUCTION COMPLETE',NAME,flush=True)
