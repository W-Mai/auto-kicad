"""Widen one existing route selected for clearance; verify the owned populated PCB copy."""
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import sys
from stdio_rpc import Rpc

def without_revisions(value):
    if isinstance(value,dict): return {k:without_revisions(v) for k,v in value.items() if k not in {"observed_revision","revision"}}
    if isinstance(value,list): return [without_revisions(v) for v in value]
    return value

def point(p): return p["x"],p["y"]
def point_distance(p,a,b):
    dx,dy=b[0]-a[0],b[1]-a[1]
    length=dx*dx+dy*dy
    t=max(0,min(1,((p[0]-a[0])*dx+(p[1]-a[1])*dy)/length)) if length else 0
    return math.hypot(p[0]-a[0]-t*dx,p[1]-a[1]-t*dy)
def segment_distance(a,b,c,d):
    def cross(p,q,r): return (q[0]-p[0])*(r[1]-p[1])-(q[1]-p[1])*(r[0]-p[0])
    overlaps=all(max(min(a[i],b[i]),min(c[i],d[i]))<=min(max(a[i],b[i]),max(c[i],d[i])) for i in (0,1))
    if overlaps and cross(a,b,c)*cross(a,b,d)<=0 and cross(c,d,a)*cross(c,d,b)<=0: return 0
    return min(point_distance(a,c,d),point_distance(b,c,d),point_distance(c,a,b),point_distance(d,a,b))
def net(item): return (item.get("net") or {}).get("name")

def select(snapshot):
    obstacles=[]
    for fp in snapshot["native_footprints"]["placements"]:
        angle=-math.radians(fp["orientation_mdeg"]/1000)
        for pad in fp["pads"]:
            x,y=pad["position"]["x_nm"],pad["position"]["y_nm"]
            if fp["side"]=="Back": y=-y
            center=(fp["position"]["x"]+x*math.cos(angle)-y*math.sin(angle),fp["position"]["y"]+x*math.sin(angle)+y*math.cos(angle))
            radius=max((math.hypot(s["size"]["x_nm"],s["size"]["y_nm"])/2+math.hypot(s["offset"]["x_nm"],s["offset"]["y_nm"])+abs(s["trapezoid_delta"]["x_nm"])+abs(s["trapezoid_delta"]["y_nm"]) for s in pad["copper_shapes"]),default=0)
            obstacles.append((net(pad),pad["layers"],center,radius))
    routing=snapshot["native_routing"]
    for via in routing["vias"]:
        radius=max(math.hypot(s["size"]["x_nm"],s["size"]["y_nm"])/2 for s in via["pad_stack"]["copper_shapes"])
        obstacles.append((net(via),via["pad_stack"]["layers"],point(via["position"]),radius))
    candidates=[]
    for track in routing["tracks"]:
        if track["lock_state"]!="Unlocked" or not net(track) or "Segment" not in track["geometry"]: continue
        geom=track["geometry"]["Segment"];a,b=point(geom["start"]),point(geom["end"])
        length=math.dist(a,b)
        if length<1000000: continue
        width=track["width_nm"]+50000
        clearance=2000000000
        for name,layers,center,radius in obstacles:
            if name==net(track) or track["layer"] not in layers: continue
            clearance=min(clearance,point_distance(center,a,b)-radius-width/2)
        for other in routing["tracks"]:
            if net(other)==net(track) or other["layer"]!=track["layer"]: continue
            if "Segment" not in other["geometry"]:
                clearance=-1;break
            g=other["geometry"]["Segment"]
            clearance=min(clearance,segment_distance(a,b,point(g["start"]),point(g["end"]))-(width+other["width_nm"])/2)
        if clearance>=400000: candidates.append((clearance,length,track["source_uuid"],track,width))
    assert candidates,"no conservative widening candidate; inspect source without modifying it"
    clearance,length,_,track,width=max(candidates,key=lambda c:c[:3])
    return track,width,dict(estimated_foreign_copper_clearance_nm=round(clearance),length_nm=round(length),candidate_count=len(candidates))

def main():
    endpoint,board_arg,output_arg,binary=sys.argv[1:]
    board=Path(board_arg);output=Path(output_arg)
    records=[]
    rpc=Rpc([binary],env=dict(os.environ,AUTO_KICAD_STATE_DIR=str(output/"state")),log=str(output/"stdio.log"))
    def call(name,args):
        response=rpc.call("tools/call",{"name":name,"arguments":args})
        assert not response.get("isError"),response
        value=response["structuredContent"];records.append({"tool":name,"response":value});(output/"mcp.json").write_text(json.dumps(records,indent=2)+"\n")
        assert value["ok"],value
        return value["data"]
    try:
        rpc.call("initialize",{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"populated-copy-edit-acceptance","version":"1"}})
        rpc.send({"jsonrpc":"2.0","method":"notifications/initialized"})
        before=call("kicad_inspect_project",{"endpoint":endpoint})["pcb"]
        track,width,selection=select(before["snapshot"])
        selection.update(source_uuid=track["source_uuid"],net=net(track),layer=track["layer"],before_width_nm=track["width_nm"],after_width_nm=width,geometry=track["geometry"])
        (output/"selection.json").write_text(json.dumps(selection,indent=2)+"\n")
        baseline=call("kicad_validate_project",{"endpoint":endpoint})
        saved=board.read_bytes();protected={board.with_suffix(ext):board.with_suffix(ext).read_bytes() if board.with_suffix(ext).exists() else None for ext in (".kicad_pro",".kicad_sch",".kicad_dru")}
        planned=call("kicad_plan_change",dict(endpoint=endpoint,source_run_id="populated-copy",request_id="widen-existing-route",edit_existing=dict(observed_revision=before["revision"],changes=[dict(kind="track",target_id=track["track_id"],source_uuid=track["source_uuid"],width_nm=width)])))
        params={k:planned[k] for k in ("plan_id","plan_hash")}
        preview=call("kicad_preview_change",params)
        assert preview["shadow_validation"]["normalized_snapshot_matches"] and preview["shadow_validation"]["shadow_cleaned"]
        assert board.read_bytes()==saved,"preview changed source"
        approval=dict(params,**{k:preview[k] for k in ("approval_hash","validation_evidence_hash")})
        committed=call("kicad_commit_change",approval);assert committed["state"]=="committed"
        after=call("kicad_inspect_project",{"endpoint":endpoint})["pcb"]
        expected=copy.deepcopy(before["snapshot"])
        next(t for t in expected["native_routing"]["tracks"] if t["source_uuid"]==track["source_uuid"])["width_nm"]=width
        assert without_revisions(after["snapshot"])==without_revisions(expected)
        final=call("kicad_validate_project",{"endpoint":endpoint,"baseline_id":baseline["baseline_id"]})
        assert not final["delta"]["introduced"]
        assert final["report"]["unconnected_items"]<=baseline["report"]["unconnected_items"]
        assert all((p.read_bytes() if p.exists() else None)==v for p,v in protected.items())
        result=dict(passed=True,selection=selection,baseline_drc=baseline["report"],final_drc=final,source_before_sha256=hashlib.sha256(saved).hexdigest(),source_after_sha256=hashlib.sha256(board.read_bytes()).hexdigest(),footprints=len(expected["native_footprints"]["placements"]),pads=sum(len(f["pads"]) for f in expected["native_footprints"]["placements"]),tracks=len(expected["native_routing"]["tracks"]),vias=len(expected["native_routing"]["vias"]),protected_files_unchanged=True,source_isolation_verified=True)
        (output/"result.json").write_text(json.dumps(result,indent=2)+"\n")
        print(json.dumps({"passed":True,"selection":selection}))
    finally: rpc.close()

if __name__=="__main__": main()
