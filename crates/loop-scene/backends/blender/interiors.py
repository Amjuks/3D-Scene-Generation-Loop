"""Room shells and detailed furnishings; no scene layout is hard-coded here."""
import random
import hashlib
import bpy

RECIPES={'room_shell','sofa','bed','rug','bookshelf','desk','bench','art','kitchen_counter','light'}

def generate(node,root,base,box,cylinder,bounds,mat):
    a,b,s,c=bounds(node);recipe=node['generation']['recipe'];made=[]
    # Stable on resume, but not the same book/art pattern for every scene's item-01.
    seed_key=f"{node['node_id']}:{node['generation'].get('seed',0)}:{node.get('design','')}"
    seed=int(hashlib.sha256(seed_key.encode()).hexdigest()[:8],16);rng=random.Random(seed)
    def material(label,color,rough=.5,metal=0):
        m=mat({'material_id':node['node_id']+'.'+label,'base_color':list(color)+[1], 'roughness':rough,'metallic':metal})
        if label in {'fabric','timber','rug'}:
            nodes=m.node_tree.nodes;noise=nodes.new('ShaderNodeTexNoise');noise.inputs['Scale'].default_value=95 if label!='timber' else 12
            bump=nodes.new('ShaderNodeBump');bump.inputs['Strength'].default_value=.14;bump.inputs['Distance'].default_value=.015
            m.node_tree.links.new(noise.outputs['Fac'],bump.inputs['Height']);m.node_tree.links.new(bump.outputs['Normal'],nodes.get('Principled BSDF').inputs['Normal'])
        return m
    wood=material('timber',[.19,.085,.034],.4);metal=material('metal',[.10,.12,.14],.28,.8)
    fabric=material('fabric',node['materials'][0]['base_color'][:3],.86)
    cream=material('linen',[.72,.66,.51],.83);dark=material('dark',[.024,.032,.04],.3)
    def part(label,size,pos,m=base,tag=None):
        o=box(node['node_id']+'.'+label,size,pos,m,root,min(.025,min(size)*.14));made.append(o)
        if tag:o['cutaway']=tag
        return o
    def local(label,sz,p,m=base,tag=None):
        return part(label,[sz[i]*s[i] for i in range(3)],[a[i]+p[i]*s[i] for i in range(3)],m,tag)
    def legs(top=.2):
        for x in [.09,.91]:
            for y in [.09,.91]:local('leg',[.055,.055,top],[x,y,top/2],wood)
    if recipe=='room_shell':
        wall=min(.18,s[0]*.035,s[1]*.035);floor=.18
        params=node['generation'].get('parameters',{})
        rooms=params.get('rooms',[])
        circulation=rooms[0].get('circulation','cross') if len(rooms)==1 else params.get('circulation','cross')
        planks=max(8,int(s[0]/.22))
        for i in range(planks):part('floor_plank',[s[0]/planks-.004,s[1],floor],[a[0]+(i+.5)*s[0]/planks,c[1],a[2]+floor/2],wood)
        part('ceiling',[s[0],s[1],.15],[c[0],c[1],b[2]-.075],cream,'roof')
        # Axial visitor routes need only their two corresponding doorways.
        for axis in [0,1]:
            along=1-axis;length=s[along];door=min(1.25,length*.24);door_h=min(2.25,s[2]-.35)
            if (circulation=='spine_x' and axis==1) or (circulation=='spine_y' and axis==0):door=0
            for side in [-1,1]:
                face=a[axis]+wall/2 if side<0 else b[axis]-wall/2
                tag='front' if axis==1 and side<0 else 'left' if axis==0 and side<0 else None
                def panel(lo,hi,z0,z1,label,m=base):
                    if hi<=lo or z1<=z0:return
                    dims=[wall,wall,z1-z0];dims[along]=hi-lo
                    pos=c.copy();pos[axis]=face;pos[along]=(lo+hi)/2;pos[2]=(z0+z1)/2
                    part(label,dims,pos,m,tag)
                center=c[along];bottom=a[2]+floor;top=b[2]-.15
                panel(center-door/2,center+door/2,bottom+door_h,top,'door_lintel')
                for sign in [-1,1]:
                    low=a[along] if sign<0 else center+door/2
                    high=center-door/2 if sign<0 else b[along]
                    window_lo=low+(high-low)*.25;window_hi=low+(high-low)*.78
                    sill=bottom+.95;head=min(top-.3,bottom+2.25)
                    panel(low,window_lo,bottom,top,'window_pier');panel(window_hi,high,bottom,top,'window_pier')
                    panel(window_lo,window_hi,bottom,sill,'window_sill');panel(window_lo,window_hi,head,top,'window_lintel')
                    panel(window_lo-.025,window_lo+.025,sill,head,'frame',wood);panel(window_hi-.025,window_hi+.025,sill,head,'frame',wood)
                    panel(window_lo,window_hi,sill,sill+.045,'frame',wood);panel(window_lo,window_hi,head-.045,head,'frame',wood)
                    panel(low,high,bottom,bottom+.10,'skirting',wood)
        # Explicit multi-room allocations may need interior partitions/floors.
        for room in node['generation'].get('parameters',{}).get('rooms',[]):
            rp=room['position'];rs=room['size']
            if rp[2]>.25:part('room_floor',[rs[0],rs[1],.16],[rp[0],rp[1],rp[2]-.08],wood)
            for axis in [0,1]:
                for sign in [-1,1]:
                    coord=rp[axis]+sign*(rs[axis]/2+.075)
                    if abs(coord-c[axis])>s[axis]/2-.45:continue
                    along=1-axis;span=rs[along];door=1.2
                    route=room.get('circulation','cross')
                    if (route=='spine_x' and axis==1) or (route=='spine_y' and axis==0):door=0
                    for off in [-1,1]:
                        dims=[.15,.15,rs[2]];dims[along]=(span-door)/2
                        pos=[rp[0],rp[1],rp[2]+rs[2]/2];pos[axis]=coord;pos[along]+=off*(span+door)/4
                        part('partition',dims,pos,base,'front' if axis==1 and sign<0 else None)
    elif recipe in {'sofa','bench'}:
        legs(.16);local('base',[.96,.90,.20],[.5,.5,.23],wood)
        if recipe=='sofa':
            local('back',[.96,.15,.66],[.5,.90,.66],fabric)
            for x in [.055,.945]:local('arm',[.11,.94,.43],[x,.5,.44],fabric)
        for i in range(3):
            local('seat_cushion',[.255,.70,.20],[(i+.5)/3,.44,.41],fabric)
            if recipe=='sofa':local('back_cushion',[.255,.12,.39],[(i+.5)/3,.76,.70],cream)
    elif recipe=='bed':
        legs(.18);local('frame',[.98,.95,.20],[.5,.5,.22],wood)
        local('headboard',[1,.06,.84],[.5,.97,.57],wood)
        local('mattress',[.91,.89,.24],[.5,.47,.42],cream)
        local('duvet',[.93,.59,.09],[.5,.30,.56],fabric)
        for x in [.27,.73]:local('pillow',[.38,.20,.10],[x,.77,.59],cream)
        for i in range(5):local('quilt_seam',[.004,.57,.005],[.17+i*.16,.30,.609],cream)
    elif recipe=='rug':
        local('woven_rug',[1,1,.7],[.5,.5,.35],fabric)
        for x in [.04,.96]:local('border',[.04,.94,.1],[x,.5,.75],cream)
        for y in [.04,.96]:local('border',[.90,.04,.1],[.5,y,.75],cream)
    elif recipe=='bookshelf':
        local('back',[1,.045,1],[.5,.975,.5],wood)
        for x in [.025,.975]:local('side',[.05,1,1],[x,.5,.5],wood)
        colors=[[.18,.28,.34],[.48,.15,.10],[.56,.40,.16],[.21,.33,.19],[.62,.52,.37]]
        bookmats=[material('book'+str(i),v,.78) for i,v in enumerate(colors)]
        for row in range(5):
            local('shelf',[.95,1,.025],[.5,.5,.025+row*.235],wood)
            if row==4:continue
            for j in range(10):
                height=rng.uniform(.13,.19)
                local('book',[.058,.60,height],[.12+j*.081,.50,.05+row*.235+height/2],rng.choice(bookmats))
                local('spine_band',[.057,.006,.012],[.12+j*.081,.195,.09+row*.235],cream)
    elif recipe=='desk':
        legs(.60);local('desktop',[1,1,.055],[.5,.5,.63],wood)
        local('pedestal',[.22,.85,.57],[.16,.5,.32],base)
        for z in [.2,.38,.55]:local('drawer_pull',[.10,.04,.016],[.16,.04,z],metal)
        local('monitor_base',[.26,.24,.025],[.60,.75,.68],metal)
        local('monitor_stem',[.04,.045,.13],[.60,.78,.75],metal)
        local('monitor',[.46,.055,.24],[.60,.79,.88],dark)
        local('screen',[.42,.008,.20],[.60,.759,.88],material('screen',[.12,.22,.30],.2))
        local('keyboard',[.35,.22,.025],[.60,.33,.675],dark)
        for i in range(3):local('notebook',[.20,.26,.014],[.18,.55,.676+i*.015],cream)
    elif recipe=='art':
        local('frame',[1,1,1],[.5,.5,.5],wood);local('canvas',[.88,.15,.88],[.5,.10,.5],cream)
        for i in range(7):local('abstract_shape',[.07,.05,rng.uniform(.15,.6)],[.18+i*.105,.025,.5],material('paint'+str(i),[rng.uniform(.1,.6),rng.uniform(.1,.4),rng.uniform(.1,.3)],.8))
    elif recipe=='kitchen_counter':
        local('carcass',[.97,.94,.78],[.5,.5,.40],wood)
        local('stone_worktop',[1,1,.055],[.5,.5,.82],cream)
        for x in [.17,.5,.83]:
            local('door',[.31,.045,.68],[x,.03,.41],base);local('pull',[.14,.035,.02],[x,.02,.68],metal)
        local('sink',[.33,.48,.035],[.72,.55,.85],metal)
        local('basin',[.27,.40,.012],[.72,.55,.872],dark)
        local('tap',[.02,.035,.13],[.72,.86,.92],metal)
        local('hob',[.28,.48,.018],[.25,.55,.86],dark)
    elif recipe=='light':
        made.append(cylinder(node['node_id']+'.base',min(s[:2])*.42,s[2]*.04,[c[0],c[1],a[2]+s[2]*.02],metal,root))
        made.append(cylinder(node['node_id']+'.stem',min(s[:2])*.04,s[2]*.70,[c[0],c[1],a[2]+s[2]*.39],metal,root))
        glow=material('shade',[.9,.66,.32],.65);shader=glow.node_tree.nodes.get('Principled BSDF');shader.inputs['Emission Color'].default_value=(1,.63,.25,1);shader.inputs['Emission Strength'].default_value=1.5
        made.append(cylinder(node['node_id']+'.shade',min(s[:2])*.45,s[2]*.24,[c[0],c[1],a[2]+s[2]*.85],glow,root))
    for o in made:o['node_id']=node['node_id'];o['semantic_kind']=node['kind']
    return made
