"""Bounded architectural recipes. No model-authored code is executed."""
import math
import random
import bpy

RECIPES = {"ring", "gallery", "bridge", "arcade", "garden", "waterfall", "hologram", "light_beam"}


def material(name, color, metallic=0, emission=0, glass=False):
    m = bpy.data.materials.get(name) or bpy.data.materials.new(name)
    m.use_nodes = True
    p = m.node_tree.nodes.get("Principled BSDF")
    p.inputs["Base Color"].default_value = (*color[:3], 1)
    p.inputs["Metallic"].default_value = metallic
    p.inputs["Roughness"].default_value = .2 if glass else .38
    if glass:
        p.inputs["Transmission Weight"].default_value = .85
        p.inputs["IOR"].default_value = 1.45
    if emission:
        p.inputs["Emission Color"].default_value = (*color[:3], 1)
        p.inputs["Emission Strength"].default_value = emission
    return m


def generate(node, root, base, box, cylinder, bounds):
    a, b, s, c = bounds(node)
    name = node["node_id"]
    recipe = node["generation"]["recipe"]
    made = []
    trim = material("arch_metal", (.23,.28,.32), .8)
    glow = material("arch_cyan", (.08,.65,1), .3, 4)
    glass = material("arch_glass", (.4,.7,.8), glass=True)
    stone = material("arch_stone", (.55,.48,.36))

    def block(label, size, center, mat=base, bevel=.015):
        o = box(name+"."+label, size, center, mat, root, bevel)
        made.append(o)
        return o

    def ring(label, rx, ry, z, width, height, mat):
        # Closed annular prism, with a genuine open central void.
        vertices, faces = [], []
        count = 128
        for dz, radius in [(0,1),(0,1-width),(height,1),(height,1-width)]:
            vertices.extend([(c[0]+rx*radius*math.cos(i*2*math.pi/count),
                              c[1]+ry*radius*math.sin(i*2*math.pi/count), z+dz) for i in range(count)])
        for i in range(count):
            j = (i+1)%count
            faces.extend([(i,j,2*count+j,2*count+i), (count+j,count+i,3*count+i,3*count+j),
                          (2*count+i,2*count+j,3*count+j,3*count+i), (j,i,count+i,count+j)])
        mesh = bpy.data.meshes.new(name+label); mesh.from_pydata(vertices, [], faces); mesh.update()
        o = bpy.data.objects.new(name+label,mesh); bpy.context.collection.objects.link(o)
        o.data.materials.append(mat); o.parent=root; made.append(o)
        return o

    if recipe == "ring":
        thickness = min(.2,s[2]*.1)
        ring(".deck",s[0]/2,s[1]/2,a[2],.25,thickness,base)
        ring(".roof",s[0]/2,s[1]/2,b[2]-thickness,.25,thickness,base)
        ring(".inner_light",s[0]*.38,s[1]*.38,a[2]+thickness,.015,min(.05,s[2]*.025),glow)
        ring(".outer_glass",s[0]*.495,s[1]*.495,a[2]+thickness,.008,s[2]-2*thickness,glass)
        for i in range(40):
            angle=i*2*math.pi/40
            x=c[0]+s[0]*.475*math.cos(angle); y=c[1]+s[1]*.475*math.sin(angle)
            block("mullion%d"%i,[min(.08,s[0]*.01),min(.08,s[1]*.01),s[2]],[x,y,c[2]],trim)
        for i in range(12):
            angle=i*2*math.pi/12
            block("exhibit%d"%i,[s[0]*.016,s[1]*.016,s[2]*.28],
                  [c[0]+s[0]*.43*math.cos(angle),c[1]+s[1]*.43*math.sin(angle),a[2]+s[2]*.2],stone)
    elif recipe in {"gallery","bridge"}:
        parameters = node["generation"].get("parameters", {})
        rise = float(parameters.get("rise", 0)) if recipe == "bridge" else 0
        start_height = float(parameters.get("start_height", max(0, -rise))) if recipe == "bridge" else 0
        if not all(math.isfinite(v) for v in (rise, start_height)) or abs(rise) >= s[2]:
            raise ValueError("invalid bridge rise within its envelope")
        if recipe == "bridge" and abs(start_height - max(0, -rise)) > 1e-6:
            raise ValueError("bridge start height must match rise")
        # Build a constant-height section, then shear along local X, keeping
        # the endpoints on their landings and geometry within the envelope.
        if rise:
            s = list(s); b = list(b); c = list(c)
            s[2] -= abs(rise); b[2] = a[2] + s[2]; c[2] = (a[2] + b[2]) / 2
        t=min(.18,s[2]*.12)
        block("deck",[s[0],s[1],t],[c[0],c[1],a[2]+t/2])
        if recipe == "gallery":
            block("roof",[s[0],s[1],t],[c[0],c[1],b[2]-t/2])
        for side in [-1,1]:
            block("glazing%d"%side,[s[0],min(.04,s[1]*.03),s[2]*.7],
                  [c[0],c[1]+side*s[1]*.47,a[2]+s[2]*.48],glass)
            block("pathlight%d"%side,[s[0]*.98,min(.035,s[1]*.02),min(.025,s[2]*.02)],
                  [c[0],c[1]+side*s[1]*.4,a[2]+t],glow)
        for i in range(9):
            x=a[0]+s[0]*(.03+.94*i/8)
            for side in [-1,1]:
                block("frame%d_%d"%(i,side),[min(.06,s[0]*.02),min(.06,s[1]*.03),s[2]*.95],
                      [x,c[1]+side*s[1]*.47,c[2]],trim)
        if recipe == "gallery":
            for i in range(5):
                block("plinth%d"%i,[s[0]*.08,s[1]*.15,s[2]*.2],
                      [a[0]+s[0]*(.1+.2*i),c[1],a[2]+t+s[2]*.1],stone)
        if rise:
            for obj in made:
                for vertex in obj.data.vertices:
                    x = obj.location.x + vertex.co.x
                    vertex.co.z += start_height + rise * (x - a[0]) / s[0]
                obj.data.update()
    elif recipe == "arcade":
        n=max(2,min(12,round(s[0]/2)))
        block("lintel",[s[0],s[1],s[2]*.13],[c[0],c[1],b[2]-s[2]*.065])
        for i in range(n+1):
            x=a[0]+s[0]*(.045+.91*i/n)
            radius=min(s[0]*.035,s[1]*.4)
            made.append(cylinder(name+".column%d"%i,radius,s[2]*.87,[x,c[1],a[2]+s[2]*.435],base,root,32))
            block("capital%d"%i,[radius*2.2,s[1]*.95,s[2]*.07],[x,c[1],a[2]+s[2]*.82])
    elif recipe == "garden":
        block("backing",[s[0],s[1]*.15,s[2]],[c[0],b[1]-s[1]*.075,c[2]],stone)
        greens=[material("leaf%d"%i,col) for i,col in enumerate([(.04,.18,.06),(.11,.31,.08),(.2,.38,.1),(.04,.24,.14)])]
        rng=random.Random(node["generation"].get("seed",0))
        for i in range(180):
            pos=[a[0]+s[0]*(.07+.86*rng.random()),a[1]+s[1]*(.15+.5*rng.random()),a[2]+s[2]*(.06+.88*rng.random())]
            bpy.ops.mesh.primitive_ico_sphere_add(subdivisions=1,radius=1,location=pos)
            o=bpy.context.object; o.name=name+".foliage%d"%i
            o.scale=(s[0]*.04,s[1]*.12,s[2]*.035); bpy.ops.object.transform_apply(location=False,rotation=False,scale=True)
            o.data.materials.append(greens[i%4]); o.parent=root; made.append(o)
    elif recipe == "waterfall":
        water=material("flowing_water",(.11,.42,.48),glass=True)
        block("basin",[s[0],s[1],s[2]*.035],[c[0],c[1],a[2]+s[2]*.0175],stone)
        block("pool",[s[0]*.94,s[1]*.92,s[2]*.012],[c[0],c[1],a[2]+s[2]*.04],water)
        for i in range(32):
            block("fall%d"%i,[s[0]*.021,s[1]*.05,s[2]*.94],
                  [a[0]+s[0]*(.04+.92*i/31),c[1]+math.sin(i)*s[1]*.025,a[2]+s[2]*.53],water,.003)
    elif recipe == "hologram":
        block("projector",[s[0]*.8,s[1]*.8,s[2]*.08],[c[0],c[1],a[2]+s[2]*.04],trim)
        for i in range(6):
            ring(".projection%d"%i,s[0]*(.18+.04*i),s[1]*(.18+.04*i),a[2]+s[2]*(.2+.12*i),.06,s[2]*.012,glow)
    elif recipe == "light_beam":
        beam=material("light_beam_portable",(.25,.65,1),emission=1)
        p=beam.node_tree.nodes.get("Principled BSDF"); p.inputs["Alpha"].default_value=.045
        beam.surface_render_method='DITHERED'
        bpy.ops.mesh.primitive_cone_add(vertices=48,radius1=1,radius2=.08,depth=1,location=c)
        o=bpy.context.object; o.name=name+".beam"; o.scale=(s[0]/2,s[1]/2,s[2])
        bpy.ops.object.transform_apply(location=False,rotation=False,scale=True)
        o.data.materials.append(beam);o.parent=root;made.append(o)
    for o in made:
        o["node_id"]=name; o["semantic_kind"]=node["kind"]
    return made


def restore_native_effects():
    """glTF uses translucent cones; native Blender also gets participating volume."""
    for m in bpy.data.materials:
        if m.name.startswith("light_beam_portable") and m.use_nodes:
            tree=m.node_tree; output=tree.nodes.get("Material Output")
            if not output.inputs["Volume"].is_linked:
                volume=tree.nodes.new("ShaderNodeVolumePrincipled")
                volume.inputs["Density"].default_value=.015
                volume.inputs["Color"].default_value=(.35,.6,1,1)
                volume.inputs["Emission Strength"].default_value=.1
                tree.links.new(volume.outputs["Volume"],output.inputs["Volume"])
