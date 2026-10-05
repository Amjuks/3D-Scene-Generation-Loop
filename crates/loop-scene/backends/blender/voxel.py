"""Detailed block construction from model-authored assembly envelopes.

Recipes build parts, never choose the scene layout. Meshes are batched by
assembly so thousands of individual masonry/terrain blocks export efficiently.
"""
import hashlib
import math
import random
import bpy

RECIPES = {"voxel_terrain", "voxel_tower", "voxel_wall", "voxel_gatehouse",
           "voxel_house", "voxel_pyramid", "voxel_tree", "voxel_bridge",
           "voxel_waterfall", "voxel_stairs"}


class Builder:
    def __init__(self, node, root):
        self.node, self.root = node, root
        self.vertices, self.faces, self.indices = [], [], []
        self.materials = []
        self.seed = int(hashlib.sha256((node['node_id'] + str(node['generation'].get('seed', 0))).encode()).hexdigest()[:8], 16)
        self.rng = random.Random(self.seed)
        self.params = node['generation'].get('parameters', {})
        self.biome = self.params.get('biome', 'snow')
        self.a, self.b = node['bounds_local']['min'], node['bounds_local']['max']
        self.s = [self.b[i]-self.a[i] for i in range(3)]
        self.c = [(self.b[i]+self.a[i])/2 for i in range(3)]
        self.unit = min(1.2, max(.3, float(self.params.get('block_size', .65))))
        base = node.get('materials', [{}])[0].get('base_color', [.38,.42,.46,1])[:3]
        self.stone = self.palette('stone', base, 7)
        self.dark = self.palette('recess', [.055,.067,.075], 2)
        self.wood = self.palette('timber', [.25,.115,.042], 5)
        self.plank = self.palette('plank', [.48,.27,.10], 5)
        self.snow = self.palette('snow', [.84,.91,.98], 3)
        self.grass = self.palette('grass', [.15,.34,.075], 5)
        self.leaf = self.palette('foliage', [.055,.22,.065], 5)
        self.soil = self.palette('soil', [.24,.13,.065], 4)
        self.roof = self.palette('roof', self.params.get('roof_color', [.38,.08,.045]), 4)
        self.water = self.palette('water', [.07,.48,.61], 3)
        self.gold = self.palette('lantern', [1,.42,.055], 1, emission=3)

    def palette(self, name, color, count, emission=0):
        result=[]
        for i in range(count):
            m=bpy.data.materials.new(self.node['node_id']+'.'+name+str(i))
            m.use_nodes=True
            shader=m.node_tree.nodes.get('Principled BSDF')
            factor=.83+.3*(i/max(1,count-1))
            rgba=tuple(min(1,max(0,float(v)*factor)) for v in color[:3])+(1,)
            shader.inputs['Base Color'].default_value=rgba
            shader.inputs['Roughness'].default_value=.88 if name!='water' else .18
            if emission:
                shader.inputs['Emission Color'].default_value=rgba
                shader.inputs['Emission Strength'].default_value=emission
            result.append(len(self.materials)); self.materials.append(m)
        return result

    def box(self, center, size, palette, gap=0):
        # All detail is clipped to its owning envelope, never to scene bounds.
        lo=[max(self.a[i],center[i]-size[i]/2+gap/2) for i in range(3)]
        hi=[min(self.b[i],center[i]+size[i]/2-gap/2) for i in range(3)]
        if any(hi[i]-lo[i]<.001 for i in range(3)): return
        x,y,z=lo; X,Y,Z=hi; n=len(self.vertices)
        self.vertices.extend([(x,y,z),(X,y,z),(X,Y,z),(x,Y,z),(x,y,Z),(X,y,Z),(X,Y,Z),(x,Y,Z)])
        self.faces.extend([tuple(n+k for k in face) for face in [(3,2,1,0),(4,5,6,7),(0,1,5,4),(1,2,6,5),(2,3,7,6),(3,0,4,7)]])
        material=self.rng.choice(palette)
        self.indices.extend([material]*6)

    def block_grid(self, lo, hi, palette, unit=None, gap=.018):
        unit=unit or self.unit
        counts=[max(1,math.ceil((hi[i]-lo[i])/unit)) for i in range(3)]
        # Bound work for a pathological model-authored envelope.
        if math.prod(counts)>45000:
            unit*= (math.prod(counts)/45000)**(1/3)
            counts=[max(1,math.ceil((hi[i]-lo[i])/unit)) for i in range(3)]
        size=[(hi[i]-lo[i])/counts[i] for i in range(3)]
        for ix in range(counts[0]):
            for iy in range(counts[1]):
                for iz in range(counts[2]):
                    self.box([lo[0]+(ix+.5)*size[0],lo[1]+(iy+.5)*size[1],lo[2]+(iz+.5)*size[2]],size,palette,gap)

    def finish(self):
        mesh=bpy.data.meshes.new(self.node['node_id']+'.blocks')
        mesh.from_pydata(self.vertices,[],self.faces);mesh.update()
        obj=bpy.data.objects.new(self.node['node_id']+'.construction',mesh)
        bpy.context.collection.objects.link(obj);obj.parent=self.root
        for m in self.materials:mesh.materials.append(m)
        for poly,index in zip(mesh.polygons,self.indices):poly.material_index=index
        obj['node_id']=self.node['node_id'];obj['semantic_kind']=self.node['kind']
        obj['construction_blocks']=len(self.vertices)//8
        return [obj]

    def terrain(self):
        nx=max(4,min(55,round(self.s[0]/self.unit))); ny=max(4,min(55,round(self.s[1]/self.unit)))
        dx=self.s[0]/nx;dy=self.s[1]/ny
        nz=max(3,min(25,round(self.s[2]/self.unit)));dz=self.s[2]/nz
        cliff=self.biome=='desert' and self.s[2]>2 and max(self.s[:2])/min(self.s[:2])>3
        for ix in range(nx):
            for iy in range(ny):
                edge=max(abs((ix+.5)/nx*2-1),abs((iy+.5)/ny*2-1))
                bottom=int(edge*edge*nz*.94+self.rng.random()*.8)
                bottom=min(nz-2,bottom)
                ceiling=nz
                if cliff:
                    # Grounded canyon strata, not a floating island turned on
                    # its side. The stepped crest varies along the long axis.
                    along=ix/max(1,nx-1) if nx>ny else iy/max(1,ny-1)
                    ceiling=max(2,round(nz*(.68+.2*math.sin(along*8)**2+.12*math.sin(along*19)**2)))
                    bottom=0
                for iz in range(bottom,ceiling):
                    top=iz==ceiling-1
                    palette=(self.snow if self.biome=='snow' else self.grass if self.biome=='jungle' else self.stone) if top else (self.soil if self.biome=='jungle' and iz>nz-4 else self.stone)
                    self.box([self.a[0]+dx*(ix+.5),self.a[1]+dy*(iy+.5),self.a[2]+dz*(iz+.5)],[dx,dy,dz],palette,.01)

    def masonry_shell(self, lo, hi, entrance=False, windows=True, crenellations=True):
        width,depth,height=[hi[i]-lo[i] for i in range(3)]
        unit=min(self.unit,max(.25,width/7),max(.25,height/10))
        nx=max(3,round(width/unit));ny=max(3,round(depth/unit));nz=max(4,round(height/unit))
        dx=width/nx;dy=depth/ny;dz=height/nz
        wall=max(.2,min(dx,dy)*.75)
        for iz in range(nz):
            z=lo[2]+(iz+.5)*dz
            for ix in range(nx):
                x=lo[0]+(ix+.5)*dx
                for side in [0,1]:
                    if entrance and side==0 and abs(ix-(nx-1)/2)<max(1,nx*.15) and iz<max(3,nz*.38):continue
                    is_window=windows and nz*.25<iz<nz*.8 and iz%5 in (2,3) and ix%4==2
                    if is_window:continue
                    y=lo[1]+wall/2 if side==0 else hi[1]-wall/2
                    self.box([x,y,z],[dx,wall,dz],self.stone,.014)
            for iy in range(1,ny-1):
                y=lo[1]+(iy+.5)*dy
                if windows and nz*.25<iz<nz*.8 and iz%5 in (2,3) and iy%4==2:continue
                for x in [lo[0]+wall/2,hi[0]-wall/2]:self.box([x,y,z],[wall,dy,dz],self.stone,.014)
            if iz in (0,nz//2,nz-2):
                for x in [lo[0]+wall*.5,hi[0]-wall*.5]:self.box([x,(lo[1]+hi[1])/2,z],[wall*1.2,depth,dz*.38],self.dark)
        self.block_grid([lo[0],lo[1],hi[2]-dz],[hi[0],hi[1],hi[2]-dz*.65],self.plank,unit)
        if crenellations:
            for ix in range(0,nx,2):
                for y in [lo[1]+wall/2,hi[1]-wall/2]:
                    self.box([lo[0]+(ix+.5)*dx,y,hi[2]+dz*.4],[dx,wall,dz*.8],self.stone,.012)
                    if self.biome=='snow':self.box([lo[0]+(ix+.5)*dx,y,hi[2]+dz*.84],[dx,wall,dz*.08],self.snow)
            for iy in range(2,ny-1,2):
                for x in [lo[0]+wall/2,hi[0]-wall/2]:self.box([x,lo[1]+(iy+.5)*dy,hi[2]+dz*.4],[wall,dy,dz*.8],self.stone,.012)

    def tower(self, gate=False, wall=False):
        lo=self.a.copy();hi=self.b.copy();hi[2]-=min(self.unit,self.s[2]*.12)
        if gate:
            pier=self.s[0]*.26
            self.masonry_shell(lo,[lo[0]+pier,hi[1],hi[2]],windows=False)
            self.masonry_shell([hi[0]-pier,lo[1],lo[2]],hi,windows=False)
            self.block_grid([lo[0]+pier,lo[1],lo[2]+self.s[2]*.55],[hi[0]-pier,hi[1],hi[2]],self.stone)
            for i in range(9):
                self.box([lo[0]+pier+(i+.5)*(self.s[0]-2*pier)/9,lo[1]+self.unit*.25,lo[2]+self.s[2]*.45],
                         [self.unit*.13,self.unit*.2,self.s[2]*.35],self.wood)
            for x in [lo[0]+pier*.5,hi[0]-pier*.5]:
                self.box([x,lo[1]+self.unit*.1,lo[2]+self.s[2]*.4],[self.unit*.4,self.unit*.2,self.unit*.6],self.gold)
        else:
            self.masonry_shell(lo,hi,entrance=not wall,windows=not wall)
            if not wall:
                floors=max(1,min(6,int(self.params.get('floors',max(1,round(self.s[2]/3))))))
                for level in range(1,floors):
                    z=lo[2]+(hi[2]-lo[2])*level/floors
                    self.block_grid([lo[0]+self.unit,lo[1]+self.unit,z],
                                    [hi[0]-self.unit,hi[1]-self.unit,z+.12],self.plank)
        if wall:
            for i in range(1,max(2,int(self.s[0]/3))):
                self.box([self.a[0]+i*self.s[0]/max(2,int(self.s[0]/3)),self.a[1]+self.unit*.3,self.a[2]+self.s[2]*.35],
                         [self.unit,self.unit*.6,self.s[2]*.7],self.stone)

    def house(self):
        floor=self.a[2]; eave=floor+self.s[2]*.58
        u=min(self.unit,self.s[0]/8)
        flat=self.params.get('roof_style')=='flat' or self.biome=='desert'
        wall_palette=self.stone if self.biome=='desert' else self.plank
        for axis in [0,1]:
            along=1-axis; count=max(3,round(self.s[along]/u));levels=max(3,round((eave-floor)/u))
            for side in [-1,1]:
                for iz in range(levels):
                    for j in range(count):
                        door=axis==1 and side==-1 and abs(j-(count-1)/2)<1 and iz<levels*.7
                        window=iz in (levels//2,levels//2+1) and j%5 in (1,2)
                        if door or window:continue
                        pos=self.c.copy();pos[axis]+=side*(self.s[axis]/2-u*.5)
                        pos[along]=self.a[along]+(j+.5)*self.s[along]/count;pos[2]=floor+(iz+.5)*(eave-floor)/levels
                        dims=[u,u,(eave-floor)/levels];dims[along]=self.s[along]/count
                        self.box(pos,dims,wall_palette,.018)
        for x in [self.a[0]+u*.5,self.b[0]-u*.5]:
            for y in [self.a[1]+u*.5,self.b[1]-u*.5]:self.box([x,y,(floor+eave)/2],[u*.7,u*.7,eave-floor],self.wood)
        self.block_grid([self.a[0],self.a[1],floor],[self.b[0],self.b[1],floor+u*.3],self.plank)
        for z in [floor+u,eave-u*.2]:
            for y in [self.a[1]+u*.45,self.b[1]-u*.45]:self.box([self.c[0],y,z],[self.s[0],u*.7,u*.3],self.wood)
        if flat:
            self.block_grid([self.a[0],self.a[1],eave],[self.b[0],self.b[1],eave+u*.6],self.stone)
            for y in [self.a[1]+u/2,self.b[1]-u/2]:self.box([self.c[0],y,eave+u],[self.s[0],u,u],self.stone)
        else:
            n=max(5,round(self.s[0]/u))
            for i in range(n):
                x=self.a[0]+(i+.5)*self.s[0]/n
                rise=(1-abs((i+.5)/n*2-1))*self.s[2]*.4
                if rise>.05:
                    for y in [self.a[1]+u*.4,self.b[1]-u*.4]:
                        self.block_grid([x-self.s[0]/n/2,y-u*.3,eave],
                                        [x+self.s[0]/n/2,y+u*.3,eave+rise],self.plank,u)
                self.block_grid([x-self.s[0]/n/2,self.a[1],eave+rise],[x+self.s[0]/n/2,self.b[1],eave+rise+u*.35],self.roof,u,.018)
                if self.biome=='snow':self.box([x,self.c[1],eave+rise+u*.39],[self.s[0]/n,self.s[1],u*.08],self.snow)
        self.box([self.c[0]+self.s[0]*.23,self.a[1]+u*.5,floor+self.s[2]*.3],[u*.3,u*.3,u*.45],self.gold)
        for i in range(3):
            self.box([self.a[0]+u*(1+i*.8),self.b[1]-u,floor+u*.4],[u*.65,u*.75,u*.8],self.wood,.02)

    def pyramid(self):
        levels=max(4,min(12,int(self.params.get('levels',8))))
        height=self.s[2]*.76
        width=self.s[0]*.14
        for level in range(levels):
            f=1-.78*level/levels;z=self.a[2]+height*level/levels
            lo=[self.c[0]-self.s[0]*f/2,self.c[1]-self.s[1]*f/2,z]
            hi=[self.c[0]+self.s[0]*f/2,self.c[1]+self.s[1]*f/2,z+height/levels]
            # Reserve an actual stair channel; a staircase buried inside solid
            # terrace blocks does not produce a readable or usable approach.
            left=self.c[0]-width*.55;right=self.c[0]+width*.55
            back=max(lo[1],self.c[1]-self.s[1]*.10)
            for start,end in [(lo,[left,hi[1],hi[2]]),
                              ([right,lo[1],lo[2]],hi),
                              ([left,back,lo[2]],[right,hi[1],hi[2]])]:
                self.block_grid(start,end,self.stone,max(self.unit,self.s[0]/36),.022)
        n=levels*3
        for i in range(n):
            y=self.a[1]+(i+.5)*self.s[1]*.40/n
            self.box([self.c[0],y,self.a[2]+height*(i+1)/n/2],[width,self.s[1]*.40/n,height*(i+1)/n],self.stone,.01)
        shrine=[self.s[0]*.2,self.s[1]*.2,self.s[2]*.24]
        for x in [-1,1]:
            for y in [-1,1]:self.block_grid([self.c[0]+x*shrine[0]*.35-self.unit/2,self.c[1]+y*shrine[1]*.35-self.unit/2,self.a[2]+height],
                                          [self.c[0]+x*shrine[0]*.35+self.unit/2,self.c[1]+y*shrine[1]*.35+self.unit/2,self.b[2]-self.unit],self.stone)
        self.block_grid([self.c[0]-shrine[0]/2,self.c[1]-shrine[1]/2,self.b[2]-self.unit],
                        [self.c[0]+shrine[0]/2,self.c[1]+shrine[1]/2,self.b[2]],self.stone)

    def tree(self):
        kind=self.params.get('tree_type','pine' if self.biome=='snow' else 'palm' if self.biome=='desert' else 'broadleaf')
        trunk=min(self.s[0],self.s[1])*.18
        self.block_grid([self.c[0]-trunk/2,self.c[1]-trunk/2,self.a[2]],[self.c[0]+trunk/2,self.c[1]+trunk/2,self.a[2]+self.s[2]*.8],self.wood)
        if kind=='pine':
            for i in range(6):
                f=.95-i*.135;z=self.a[2]+self.s[2]*(.23+i*.115)
                self.block_grid([self.c[0]-self.s[0]*f/2,self.c[1]-self.s[1]*f/2,z],
                                [self.c[0]+self.s[0]*f/2,self.c[1]+self.s[1]*f/2,z+self.s[2]*.13],self.leaf)
                self.box([self.c[0],self.c[1],z+self.s[2]*.14],[self.s[0]*f,self.s[1]*f,self.unit*.14],self.snow)
        elif kind=='palm':
            for dx,dy in [(1,0),(-1,0),(0,1),(0,-1)]:
                for i in range(5):
                    f=(i+.5)/5
                    self.box([self.c[0]+dx*self.s[0]*.43*f,self.c[1]+dy*self.s[1]*.43*f,self.a[2]+self.s[2]*(.88-.17*f*f)],
                             [self.s[0]*.17,self.s[1]*.17,self.s[2]*.07],self.leaf)
        else:
            for ix in range(5):
                for iy in range(5):
                    for iz in range(3):
                        if (ix-2)**2+(iy-2)**2+(iz-1)**2>7:continue
                        self.box([self.a[0]+self.s[0]*(ix+.5)/5,self.a[1]+self.s[1]*(iy+.5)/5,self.a[2]+self.s[2]*(.57+.13*iz)],
                                 [self.s[0]/5,self.s[1]/5,self.s[2]*.14],self.leaf,.018)

    def bridge(self):
        rise=float(self.params.get('rise',0));start=float(self.params.get('start_height',0))
        n=max(8,math.ceil(self.s[0]/.35));dx=self.s[0]/n
        for i in range(n):
            t=(i+.5)/n;x=self.a[0]+t*self.s[0];z=self.a[2]+start+t*rise
            self.box([x,self.c[1],z+.12],[dx,self.s[1],.24],self.plank,.018)
            for side in [-1,1]:
                y=self.c[1]+side*self.s[1]*.44
                self.box([x,y,z+1.1],[dx,.12,.12],self.wood)
                self.box([x,y,z+.63],[dx,.08,.08],self.wood)
                if i%4==0 or i==n-1:self.box([x,y,z+.7],[.16,.16,1.4],self.wood)

    def waterfall(self):
        if self.s[2]<1.5:
            self.block_grid(self.a,self.b,self.water,max(self.unit,self.s[0]/30),.008);return
        count=max(4,min(24,round(self.s[0]/.35)))
        for i in range(count):
            self.box([self.a[0]+(i+.5)*self.s[0]/count,self.c[1],self.c[2]],
                     [self.s[0]/count,self.s[1]*(.3+.25*self.rng.random()),self.s[2]],self.water,.01)
        for _ in range(30):
            self.box([self.rng.uniform(self.a[0],self.b[0]),self.rng.uniform(self.a[1],self.b[1]),self.a[2]+self.s[2]*self.rng.random()*.15],
                     [self.unit*.3]*3,self.snow)

    def stairs(self):
        n=max(4,min(40,math.ceil(self.s[2]/.3)))
        for i in range(n):
            self.box([self.c[0],self.a[1]+(i+.5)*self.s[1]/n,self.a[2]+(i+1)*self.s[2]/n/2],
                     [self.s[0],self.s[1]/n,(i+1)*self.s[2]/n],self.stone,.015)


def generate(node, root):
    builder=Builder(node,root)
    recipe=node['generation']['recipe']
    if recipe=='voxel_terrain':builder.terrain()
    elif recipe=='voxel_tower':builder.tower()
    elif recipe=='voxel_wall':builder.tower(wall=True)
    elif recipe=='voxel_gatehouse':builder.tower(gate=True)
    elif recipe=='voxel_house':builder.house()
    elif recipe=='voxel_pyramid':builder.pyramid()
    elif recipe=='voxel_tree':builder.tree()
    elif recipe=='voxel_bridge':builder.bridge()
    elif recipe=='voxel_waterfall':builder.waterfall()
    elif recipe=='voxel_stairs':builder.stairs()
    else:raise ValueError('Unknown voxel recipe '+recipe)
    return builder.finish()
