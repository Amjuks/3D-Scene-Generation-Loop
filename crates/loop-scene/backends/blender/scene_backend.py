"""Loop procedural PBR v1 Blender worker.

The Rust controller owns contracts/state. This process only translates one
validated job into isolated native/interchange artifacts and measured evidence.
"""
import json, math, sys, traceback
from pathlib import Path
import bpy
from mathutils import Vector
sys.path.insert(0, str(Path(__file__).resolve().parent))
import architecture
import voxel
import interiors

def args():
    marker=sys.argv.index("--")
    return json.loads(Path(sys.argv[marker+1]).read_text())

def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    s=bpy.context.scene
    s.unit_settings.system='METRIC'; s.unit_settings.length_unit='METERS'; s.unit_settings.scale_length=1.0
    s.render.film_transparent=False
    return s

def mat(spec, suffix=""):
    name=spec.get("material_id","material")+suffix
    old=bpy.data.materials.get(name)
    if old: return old
    m=bpy.data.materials.new(name); m.use_nodes=True
    p=m.node_tree.nodes.get("Principled BSDF")
    color=spec.get("base_color",[0.5,0.5,0.5,1.0])
    p.inputs["Base Color"].default_value=color
    metallic=p.inputs.get("Metallic") or p.inputs.get("Metallic IOR Level")
    if metallic: metallic.default_value=float(spec.get("metallic",0.0))
    p.inputs["Roughness"].default_value=float(spec.get("roughness",0.5))
    description=spec.get("description", "").lower()
    if "finish:glass" in description or "finish:water" in description:
        p.inputs["Transmission Weight"].default_value=.82
        p.inputs["IOR"].default_value=1.45
    if "finish:emissive" in description:
        p.inputs["Emission Color"].default_value=color
        p.inputs["Emission Strength"].default_value=3
    m["texture_scale_m"]=float(spec.get("texture_scale_m",1.0))
    return m

def default_mat(node, color=None):
    if node.get("materials"): return mat(node["materials"][0])
    return mat({"material_id":"fallback_"+node["kind"],"base_color":color or [0.42,0.34,0.26,1],"roughness":0.48,"metallic":0.0,"texture_scale_m":1.0})

def box(name, size, center, material, parent, bevel=0.012):
    size=[max(float(x),0.002) for x in size]
    bpy.ops.mesh.primitive_cube_add(size=1, location=center)
    o=bpy.context.object; o.name=name; o.dimensions=size
    bpy.ops.object.transform_apply(location=False,rotation=False,scale=True)
    if material: o.data.materials.append(material)
    if bevel>0:
        mod=o.modifiers.new("Construction edge","BEVEL"); mod.width=min(bevel,min(size)*0.22); mod.segments=3
    o.parent=parent
    return o

def cylinder(name, radius, depth, center, material, parent, vertices=32):
    bpy.ops.mesh.primitive_cylinder_add(vertices=vertices,radius=max(radius,.002),depth=max(depth,.002),location=center)
    o=bpy.context.object;o.name=name
    if material:o.data.materials.append(material)
    b=o.modifiers.new("Edge softening","BEVEL");b.width=min(.008,radius*.15);b.segments=2
    o.parent=parent;return o

def node_root(node, parent=None, apply_transform=True):
    o=bpy.data.objects.new(node["node_id"],None);bpy.context.scene.collection.objects.link(o)
    o["node_id"]=node["node_id"];o["kind"]=node["kind"];o["revision"]=node["revision"]
    if apply_transform:
        t=node["transform"];o.location=t["translation"]
        x,y,z,w=t["rotation_xyzw"];o.rotation_mode='QUATERNION';o.rotation_quaternion=(w,x,y,z)
    o.parent=parent;return o

def bounds(node):
    a=node["bounds_local"]["min"];b=node["bounds_local"]["max"]
    return a,b,[b[i]-a[i] for i in range(3)],[(a[i]+b[i])/2 for i in range(3)]

def add_wall_with_openings(node, root, material, a, b, size, center):
    openings=node.get("construction",{}).get("openings",[])
    if not openings:return [box(node["node_id"]+".wall",size,center,material,root,.01)]
    # Wall long axis is inferred from horizontal extents. Openings use offsets
    # along that axis and bottom/width/height in meters.
    axis=0 if size[0]>=size[1] else 1; thick=1-axis; start=a[axis]; end=b[axis]
    valid=[]
    for op in openings:
        off=float(op.get("offset_m",0));w=float(op.get("width_m",0));h=float(op.get("height_m",0));bottom=float(op.get("bottom_m",0))
        lo=max(start,start+off);hi=min(end,lo+w)
        if hi>lo and h>0:valid.append((lo,hi,max(a[2],a[2]+bottom),min(b[2],a[2]+bottom+h)))
    valid.sort(); objects=[];cursor=start
    for idx,(lo,hi,bot,top) in enumerate(valid):
        if lo>cursor:
            c=center.copy();c[axis]=(cursor+lo)/2;s=size.copy();s[axis]=lo-cursor;objects.append(box(f'{node["node_id"]}.wall_{idx}_side',s,c,material,root,.008))
        if bot>a[2]:
            c=center.copy();c[axis]=(lo+hi)/2;c[2]=(a[2]+bot)/2;s=size.copy();s[axis]=hi-lo;s[2]=bot-a[2];objects.append(box(f'{node["node_id"]}.wall_{idx}_sill',s,c,material,root,.008))
        if top<b[2]:
            c=center.copy();c[axis]=(lo+hi)/2;c[2]=(top+b[2])/2;s=size.copy();s[axis]=hi-lo;s[2]=b[2]-top;objects.append(box(f'{node["node_id"]}.wall_{idx}_lintel',s,c,material,root,.008))
        cursor=max(cursor,hi)
    if cursor<end:
        c=center.copy();c[axis]=(cursor+end)/2;s=size.copy();s[axis]=end-cursor;objects.append(box(node["node_id"]+'.wall_end',s,c,material,root,.008))
    return objects

def geometry(node, root):
    kind=node["kind"].lower();a,b,size,center=bounds(node);m=default_mat(node);made=[]
    recipe=node.get("generation",{}).get("recipe",kind).lower()
    component_path=node.get("generation",{}).get("parameters",{}).get("component_glb_path")
    if component_path and Path(component_path).is_file():
        before=set(bpy.context.scene.objects);bpy.ops.import_scene.gltf(filepath=str(component_path));imported=[o for o in bpy.context.scene.objects if o not in before]
        for o in imported:
            if o.type in {'CAMERA','LIGHT'}:bpy.data.objects.remove(o,do_unlink=True);continue
            if o.parent is None:o.parent=root
            if o.type=='MESH':o["node_id"]=node["node_id"];o["semantic_kind"]=node["kind"];made.append(o)
        return made
    if node.get("child_ids") and node.get("generation",{}).get("strategy")=="assembly": return made
    if recipe in voxel.RECIPES:
        return voxel.generate(node,root)
    if recipe in interiors.RECIPES:
        return interiors.generate(node,root,m,box,cylinder,bounds,mat)
    if recipe in architecture.RECIPES:
        return architecture.generate(node,root,m,box,cylinder,bounds)
    if any(x in recipe or x in kind for x in ["wall","boundary"]):
        return add_wall_with_openings(node,root,m,a,b,size,center)
    if any(x in recipe or x in kind for x in ["slab","floor","ceiling","roof"]):
        made.append(box(node["node_id"]+".structure",size,center,m,root,.006))
    elif "table" in recipe or "table" in kind:
        top=min(size[2]*.35,max(.002,min(.07,size[2]*.12))); z=b[2]-top/2
        made.append(box(node["node_id"]+".top",[size[0],size[1],top],[center[0],center[1],z],m,root,.018))
        leg=min(min(size[0],size[1])*.2,max(.002,min(size[0],size[1])*.07)); inset=leg*.8
        for ix,x in enumerate([a[0]+inset,b[0]-inset]):
            for iy,y in enumerate([a[1]+inset,b[1]-inset]):made.append(box(f'{node["node_id"]}.leg_{ix}_{iy}',[leg,leg,size[2]-top],[x,y,a[2]+(size[2]-top)/2],m,root,.008))
    elif "chair" in recipe or "chair" in kind:
        seat_z=a[2]+size[2]*.48;seat_t=max(.025,size[2]*.05);made.append(box(node["node_id"]+".seat",[size[0]*.86,size[1]*.82,seat_t],[center[0],center[1],seat_z],m,root,.018));leg=max(.022,size[0]*.075)
        for ix,x in enumerate([a[0]+size[0]*.12,b[0]-size[0]*.12]):
            for iy,y in enumerate([a[1]+size[1]*.12,b[1]-size[1]*.12]):made.append(box(f'{node["node_id"]}.leg_{ix}_{iy}',[leg,leg,seat_z-a[2]],[x,y,a[2]+(seat_z-a[2])/2],m,root,.006))
        made.append(box(node["node_id"]+".back",[size[0]*.82,max(.025,size[1]*.09),b[2]-seat_z],[center[0],b[1]-size[1]*.08,(seat_z+b[2])/2],m,root,.018))
    elif any(x in recipe or x in kind for x in ["cabinet","wardrobe","cupboard"]):
        t=max(.018,min(size)*.04);made += [box(node["node_id"]+".left",[t,size[1],size[2]],[a[0]+t/2,center[1],center[2]],m,root,.006),box(node["node_id"]+".right",[t,size[1],size[2]],[b[0]-t/2,center[1],center[2]],m,root,.006),box(node["node_id"]+".top",[size[0],size[1],t],[center[0],center[1],b[2]-t/2],m,root,.006),box(node["node_id"]+".bottom",[size[0],size[1],t],[center[0],center[1],a[2]+t/2],m,root,.006)]
        door=mat({"material_id":"door_"+node["node_id"],"base_color":[.32,.2,.1,1],"roughness":.38,"metallic":0,"texture_scale_m":1})
        made += [box(node["node_id"]+".door_l",[size[0]*.48,t,size[2]*.9],[center[0]-size[0]*.245,a[1]+t*1.5,center[2]],door,root,.01),box(node["node_id"]+".door_r",[size[0]*.48,t,size[2]*.9],[center[0]+size[0]*.245,a[1]+t*1.5,center[2]],door,root,.01)]
        metal=mat({"material_id":"brushed_metal","base_color":[.3,.32,.34,1],"roughness":.25,"metallic":.8,"texture_scale_m":.2})
        made += [cylinder(node["node_id"]+".handle_l",.012,size[2]*.18,[center[0]-.035,a[1]+.013,center[2]],metal,root,16),cylinder(node["node_id"]+".handle_r",.012,size[2]*.18,[center[0]+.035,a[1]+.013,center[2]],metal,root,16)]
    elif "shelf" in recipe or "shelf" in kind:
        t=max(.018,size[0]*.035);made += [box(node["node_id"]+".side_l",[t,size[1],size[2]],[a[0]+t/2,center[1],center[2]],m,root,.006),box(node["node_id"]+".side_r",[t,size[1],size[2]],[b[0]-t/2,center[1],center[2]],m,root,.006)]
        count=int(node.get("construction",{}).get("shelf_count",4));
        for i in range(count+1):made.append(box(f'{node["node_id"]}.shelf_{i}',[size[0],size[1],t],[center[0],center[1],a[2]+t/2+i*(size[2]-t)/max(1,count)],m,root,.006))
    elif "stair" in recipe or "stair" in kind:
        count=max(2,int(node.get("construction",{}).get("step_count",round(size[2]/.18))));run=size[1]/count;rise=size[2]/count
        for i in range(count):made.append(box(f'{node["node_id"]}.tread_{i}',[size[0],run,max(.04,rise)],[center[0],a[1]+run*(i+.5),a[2]+rise*(i+.5)],m,root,.006))
    elif "window" in recipe or "window" in kind:
        frame=max(.025,min(size[0],size[2])*.08);made += [box(node["node_id"]+".frame_l",[frame,size[1],size[2]],[a[0]+frame/2,center[1],center[2]],m,root,.006),box(node["node_id"]+".frame_r",[frame,size[1],size[2]],[b[0]-frame/2,center[1],center[2]],m,root,.006),box(node["node_id"]+".frame_t",[size[0],size[1],frame],[center[0],center[1],b[2]-frame/2],m,root,.006),box(node["node_id"]+".frame_b",[size[0],size[1],frame],[center[0],center[1],a[2]+frame/2],m,root,.006)]
        glass=mat({"material_id":"glass","base_color":[.16,.3,.42,.28],"roughness":.08,"metallic":0,"texture_scale_m":1});glass.surface_render_method='DITHERED';p=glass.node_tree.nodes.get("Principled BSDF");p.inputs["Transmission Weight"].default_value=.82;made.append(box(node["node_id"]+".glass",[size[0]-2*frame,max(.006,size[1]*.18),size[2]-2*frame],center,glass,root,.002))
    elif "door" in recipe or "door" in kind:
        t=size[1]*.45;made.append(box(node["node_id"]+".panel",[size[0]*.92,t,size[2]*.94],[center[0],center[1],a[2]+size[2]*.47],m,root,.012));metal=mat({"material_id":"door_hardware","base_color":[.22,.2,.16,1],"roughness":.2,"metallic":.9,"texture_scale_m":.1});made.append(cylinder(node["node_id"]+".handle",min(.025,size[1]*.12),min(.12,size[2]*.1),[b[0]-size[0]*.18,a[1]+size[1]*.13,a[2]+size[2]*.52],metal,root,24))
    elif any(x in recipe or x in kind for x in ["plant","tree","vegetation"]):
        pot=mat({"material_id":"terracotta","base_color":[.42,.13,.06,1],"roughness":.72,"metallic":0,"texture_scale_m":.3});green=mat({"material_id":"foliage","base_color":[.05,.25,.07,1],"roughness":.65,"metallic":0,"texture_scale_m":.25});made.append(cylinder(node["node_id"]+".pot",min(size[0],size[1])*.28,size[2]*.25,[center[0],center[1],a[2]+size[2]*.125],pot,root,32));made.append(cylinder(node["node_id"]+".stem",min(size[0],size[1])*.035,size[2]*.62,[center[0],center[1],a[2]+size[2]*.5],m,root,16));
        for i in range(9):
            ang=i*2.399;z=a[2]+size[2]*(.45+.05*i);r=min(min(size[0],size[1])*(.08+.014*i),size[2]*.12);bpy.ops.mesh.primitive_ico_sphere_add(subdivisions=2,radius=r,location=[center[0]+math.cos(ang)*r,center[1]+math.sin(ang)*r,z]);o=bpy.context.object;o.name=f'{node["node_id"]}.leaf_{i}';o.scale=(1,.55,.35);bpy.ops.object.transform_apply(location=False,rotation=False,scale=True);o.data.materials.append(green);o.parent=root;made.append(o)
    elif "light" in recipe or "lamp" in kind:
        made.append(cylinder(node["node_id"]+".stand",min(size[0],size[1])*.05,size[2]*.8,[center[0],center[1],a[2]+size[2]*.4],m,root,24));bpy.ops.mesh.primitive_uv_sphere_add(segments=32,ring_count=16,radius=min(min(size[0],size[1])*.35,size[2]*.12),location=[center[0],center[1],b[2]-size[2]*.12]);o=bpy.context.object;o.name=node["node_id"]+".shade";o.data.materials.append(m);o.parent=root;made.append(o)
    else:
        made.append(box(node["node_id"]+".body",size,center,m,root,max(.004,min(size)*.035)))
        # A distinct contact/plinth detail prevents silent featureless fallback.
        if size[2]>.12:made.append(box(node["node_id"]+".contact",[size[0]*.82,size[1]*.82,min(.025,size[2]*.08)],[center[0],center[1],a[2]+min(.0125,size[2]*.04)],m,root,.004))
    for o in made:o["node_id"]=node["node_id"];o["semantic_kind"]=node["kind"]
    return made

def inspect_scene():
    deps=bpy.context.evaluated_depsgraph_get();mins=Vector((math.inf,)*3);maxs=Vector((-math.inf,)*3);tris=0;ids=set();mats=set();count=0
    for o in bpy.context.scene.objects:
        if o.type!='MESH':continue
        count+=1;eo=o.evaluated_get(deps);mesh=eo.to_mesh();tris+=sum(max(0,len(p.vertices)-2) for p in mesh.polygons)
        for c in eo.bound_box:
            p=eo.matrix_world@Vector(c);mins=Vector((min(mins[i],p[i]) for i in range(3)));maxs=Vector((max(maxs[i],p[i]) for i in range(3)))
        ids.add(str(o.get("node_id",o.name.split('.')[0])));mats.update(x.name for x in o.data.materials if x);eo.to_mesh_clear()
    finite=count>0 and all(math.isfinite(x) for x in [*mins,*maxs])
    return {"mesh_count":count,"triangles":tris,"bounds_canonical":{"min":list(mins) if finite else None,"max":list(maxs) if finite else None},"semantic_node_ids":sorted(ids),"materials":sorted(mats),"object_names":sorted(o.name for o in bpy.context.scene.objects if o.type=='MESH'),"all_object_names":sorted(o.name for o in bpy.context.scene.objects),"nonempty":finite}

def lighting(scene, voxel_scene=False, radius=20):
    world=bpy.data.worlds.new("Photographic World");scene.world=world;world.use_nodes=True;world.node_tree.nodes["Background"].inputs["Color"].default_value=(.09,.12,.16,1);world.node_tree.nodes["Background"].inputs["Strength"].default_value=.32
    data=bpy.data.lights.new("Sun","SUN");data.energy=2.3;data.angle=math.radians(8);sun=bpy.data.objects.new("Sun",data);scene.collection.objects.link(sun);sun.rotation_euler=(math.radians(28),math.radians(-18),math.radians(-32))
    area_data=bpy.data.lights.new("Sky fill","AREA");area_data.energy=900;area_data.shape='DISK';area_data.size=8;area=bpy.data.objects.new("Sky fill",area_data);scene.collection.objects.link(area);area.location=(2,-4,7);area.rotation_euler=(.25,0,.2)
    if voxel_scene:
        world.node_tree.nodes["Background"].inputs["Color"].default_value=(.34,.46,.62,1)
        world.node_tree.nodes["Background"].inputs["Strength"].default_value=.65
        data.angle=math.radians(15)
        area_data.energy=radius*radius*18;area_data.size=radius
        area.location=(radius*.4,-radius*.7,radius*1.4)
        aim(area,(0,0,0))

def configure_render(scene,job):
    name=job.get("renderer","eevee");scene.render.engine={"eevee":"BLENDER_EEVEE_NEXT","cycles":"CYCLES","workbench":"BLENDER_WORKBENCH"}.get(name,"BLENDER_EEVEE_NEXT")
    if scene.render.engine=='CYCLES':scene.cycles.samples=int(job.get("samples",32));scene.cycles.device='CPU';scene.cycles.use_denoising=True
    w,h=job.get("resolution",[1280,720]);scene.render.resolution_x=int(w);scene.render.resolution_y=int(h);scene.render.resolution_percentage=100;scene.render.image_settings.file_format='PNG';scene.view_settings.look='AgX - Medium High Contrast'

def aim(camera,target):camera.rotation_euler=(Vector(target)-camera.location).to_track_quat('-Z','Y').to_euler()
def camera_plan(report,nodes,out):
    a=Vector(report["bounds_canonical"]["min"]);b=Vector(report["bounds_canonical"]["max"]);c=(a+b)*.5;s=b-a;radius=max(s.x,s.y,s.z,1)
    plans=[("exterior_three_quarter",c+Vector((-1.25*radius,-1.45*radius,.85*radius)),c),("north_elevation",c+Vector((0,1.8*radius,.35*radius)),c),("overhead",c+Vector((0,0,2.15*radius)),c)]
    focal=[n for n in nodes if n.get('generation',{}).get('recipe') in
           {'voxel_house','voxel_pyramid','voxel_tower','voxel_gatehouse'} and max(bounds(n)[2])>=3]
    if focal:
        # Test actual evaluated geometry, not just bounding boxes: terrain and
        # surrounding buildings can otherwise hide the intended architecture.
        deps=bpy.context.evaluated_depsgraph_get()
        candidates=[c+Vector((x*radius,y*radius,z*radius))
                    for z in [.85,1.3] for x,y in [(-1.25,-1.45),(1.25,-1.45),(1.25,1.45),(-1.25,1.45)]]
        def visibility(pos):
            score=0
            for n in focal:
                na,nb,ns,nc=bounds(n);t=Vector(n['transform']['translation'])
                weight=math.sqrt(ns[0]*ns[1]*ns[2])
                for height in [.25,.55,.85]:
                    target=t+Vector((nc[0],nc[1],na[2]+ns[2]*height))
                    delta=target-pos
                    hit,loc,normal,index,obj,matrix=bpy.context.scene.ray_cast(deps,pos,delta.normalized(),distance=delta.length+.1)
                    if hit and obj.get('node_id')==n['node_id']:score+=weight
            return score
        hero=max(candidates,key=visibility)
        plans[0]=(plans[0][0],hero,c)
        main=max(focal,key=lambda n:math.prod(bounds(n)[2]))
        ma,mb,ms,mc=bounds(main);target=Vector(mc)+Vector(main['transform']['translation'])
        direction=(hero-c).normalized();distance=max(ms)*2.4
        plans.append(('structure_detail',target+direction*distance,target))
    for n in nodes:
        if n['kind']=='furnished_room':
            na,nb,ns,nc=bounds(n);frame=bpy.data.objects[n['node_id']].matrix_world
            pos=frame@Vector((0,na[1]+.38,min(1.65,ns[2]*.62)))
            target=frame@Vector((0,nb[1]*.35,min(1.15,ns[2]*.45)))
            plans.append(('interior_'+n['node_id'],pos,target))
            reverse=frame@Vector((0,nb[1]-.38,min(1.65,ns[2]*.62)))
            reverse_target=frame@Vector((0,na[1]*.35,min(1.15,ns[2]*.45)))
            plans.append(('interior_'+n['node_id']+'_reverse',reverse,reverse_target))
            continue
        if any(x in n["kind"].lower() for x in ["room","interior","space"]):
            na,nb,ns,nc=bounds(n);t=n["transform"]["translation"];pos=[t[0]+na[0]+ns[0]*.18,t[1]+na[1]+ns[1]*.22,t[2]+max(1.55,na[2]+ns[2]*.45)];target=[t[0]+nc[0],t[1]+nc[1],t[2]+min(nb[2],1.35)];plans.append(("interior_"+n["node_id"].replace('.','_'),Vector(pos),Vector(target)))
    if any(n['kind']=='furnished_room' for n in nodes):
        plans.append(('cutaway',plans[0][1],c))
        return plans
    return plans[:8]

def save_and_export(out):
    bpy.ops.wm.save_as_mainfile(filepath=str(out/"scene.blend"))
    bpy.ops.export_scene.gltf(filepath=str(out/"scene.glb"),export_format='GLB',export_yup=True,export_lights=True,export_cameras=True,export_extras=True,export_apply=True)

def component(job):
    out=Path(job["output_dir"]);out.mkdir(parents=True,exist_ok=True);reset();n=job["node"];root=node_root(n,None,False);geometry(n,root)
    report=inspect_scene();(out/"inspection.json").write_text(json.dumps(report,indent=2));
    bpy.ops.wm.save_as_mainfile(filepath=str(out/"component.blend"));bpy.ops.export_scene.gltf(filepath=str(out/"component.glb"),export_format='GLB',export_yup=True,export_extras=True,export_apply=True)

def assembly(job):
    out=Path(job["output_dir"]);out.mkdir(parents=True,exist_ok=True);scene=reset();nodes=job["scene"]["nodes"];roots={}
    pending=list(nodes)
    while pending:
        progress=False
        for n in pending[:]:
            pid=n.get("parent_id")
            if pid is None or pid in roots:
                roots[n["node_id"]]=node_root(n,roots.get(pid),True);geometry(n,roots[n["node_id"]]);pending.remove(n);progress=True
        if not progress:raise RuntimeError("unresolvable transform parents")
    # Asymmetric exporter fixture, hidden from beauty render but exported.
    fixture=node_root({"node_id":"__axis_fixture","kind":"diagnostic","revision":1,"transform":{"translation":[0,0,0],"rotation_xyzw":[0,0,0,1]}},None,False);fixture.hide_render=True
    for name,pos in [("X_EAST",(1.0,0,0)),("Y_NORTH",(0,2.0,0)),("Z_UP",(0,0,3.0))]:
        landmark=bpy.data.objects.new("AXIS_"+name,None);scene.collection.objects.link(landmark);landmark.location=pos;landmark.parent=fixture;landmark["canonical_landmark"]=name
    architecture.restore_native_effects()
    bpy.context.view_layer.update()
    for n in nodes:
        if n['kind']!='furnished_room':continue
        na,nb,ns,nc=bounds(n)
        data=bpy.data.lights.new(n['node_id']+'.ceiling_light','AREA')
        data.energy=ns[0]*ns[1]*7;data.size=min(ns[:2])*.65;data.color=(1,.88,.73)
        lamp=bpy.data.objects.new(data.name,data);scene.collection.objects.link(lamp)
        lamp.matrix_world=roots[n['node_id']].matrix_world.copy();lamp.location=roots[n['node_id']].matrix_world@Vector((0,0,nb[2]-.1))
    report=inspect_scene()
    is_voxel=any(n["kind"]=="voxel_scene" for n in nodes)
    radius=max(report["bounds_canonical"]["max"][i]-report["bounds_canonical"]["min"][i] for i in range(3))
    lighting(scene,is_voxel,radius);configure_render(scene,job);(out/"inspection.json").write_text(json.dumps(report,indent=2));cams=[]
    for name,pos,target in camera_plan(report,nodes,out):
        for o in scene.objects:
            if o.type=='MESH' and o.get('cutaway'):o.hide_render=(name=='cutaway')
        data=bpy.data.cameras.new(name);cam=bpy.data.objects.new(name,data);scene.collection.objects.link(cam);cam.location=pos;aim(cam,target);data.lens=30 if name.startswith("interior") else 46;data.clip_start=.03;data.clip_end=max(100,(pos-target).length*5);scene.camera=cam;path=out/(name+".png");scene.render.filepath=str(path);bpy.ops.render.render(write_still=True);cams.append({"id":name,"path":str(path),"position":list(pos),"target":list(target),"lens_mm":data.lens,"coverage":"interior" if name.startswith("interior") else "exterior_or_layout"})
    for o in scene.objects:
        if o.type=='MESH' and o.get('cutaway'):o.hide_render=False
    scene.camera=bpy.data.objects.get('exterior_three_quarter',scene.camera)
    (out/"cameras.json").write_text(json.dumps({"coordinate_system":"RH_M_ZUP_XEAST_YNORTH","gltf_mapping":"(x,y,z)->(x,z,-y)","renderer":scene.render.engine,"resolution":[scene.render.resolution_x,scene.render.resolution_y],"samples":job.get("samples",32),"cameras":cams},indent=2));save_and_export(out)

def load_report(path,kind):
    reset()
    if kind=='blend':bpy.ops.wm.open_mainfile(filepath=str(path))
    else:bpy.ops.import_scene.gltf(filepath=str(path))
    return inspect_scene()

def validate_exports(job):
    out=Path(job["output_dir"]);out.mkdir(parents=True,exist_ok=True);a=load_report(Path(job["blend"]),'blend');b=load_report(Path(job["glb"]),'glb');tol=float(job.get("translation_tolerance_m",.001));errors=[]
    if not a["nonempty"] or not b["nonempty"]:errors.append("empty geometry")
    if a["bounds_canonical"]["min"] and b["bounds_canonical"]["min"]:
        residual=max(abs(a["bounds_canonical"][k][i]-b["bounds_canonical"][k][i]) for k in ["min","max"] for i in range(3))
        if residual>tol:errors.append(f"bounds residual {residual}m > {tol}m")
    else:residual=None
    expected={"AXIS_X_EAST","AXIS_Y_NORTH","AXIS_Z_UP"};original={x.split('.')[0] for x in a.get("all_object_names",[])};have={o.name.split('.')[0] for o in bpy.context.scene.objects};missing=sorted(expected-have) if expected & original else []
    if missing:errors.append("asymmetric axis fixture missing: "+str(missing))
    # Actual GLB-side inspection render catches mirroring/material/camera failures.
    s=bpy.context.scene;lighting(s);configure_render(s,{"renderer":"eevee","resolution":[640,360],"samples":8});bb=b["bounds_canonical"];lo=Vector(bb["min"]);hi=Vector(bb["max"]);c=(lo+hi)/2;r=max(*(hi-lo),1);d=bpy.data.cameras.new("GLB inspection");cam=bpy.data.objects.new("GLB inspection",d);s.collection.objects.link(cam);cam.location=c+Vector((-r,-1.4*r,.8*r));aim(cam,c);s.camera=cam;s.render.filepath=str(out/"glb-inspection.png");bpy.ops.render.render(write_still=True)
    result={"passed":not errors,"canonical_basis":"RH meters +X east +Y north +Z up","gltf_basis_mapping":"(x,y,z)->(x,z,-y) by exporter/importer exactly once","blend":a,"glb_reimported_to_canonical":b,"max_bounds_residual_m":residual,"tolerance_m":tol,"errors":errors,"glb_render":str(out/"glb-inspection.png")};(out/"parity.json").write_text(json.dumps(result,indent=2))

def main():
    job=args();mode=job["mode"]
    if mode=='component':component(job)
    elif mode=='assembly':assembly(job)
    elif mode=='validate_exports':validate_exports(job)
    else:raise ValueError("unsupported mode "+mode)

if __name__ == "__main__":
    try:main()
    except Exception:
        traceback.print_exc();sys.exit(31)
