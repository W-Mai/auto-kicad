"""Verify exact-set deletion through production stdio MCP on an owned KiCad board."""
import argparse
import copy
import json
import os
from pathlib import Path
from stdio_rpc import Rpc

def without_revisions(value):
    if isinstance(value, dict):
        return {k:without_revisions(v) for k,v in value.items() if k not in {"observed_revision","revision"}}
    if isinstance(value, list):
        return [without_revisions(v) for v in value]
    return value

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ("binary","state","endpoint","board","evidence"): parser.add_argument(name)
    args=parser.parse_args()
    board=Path(args.board)
    transcript=[]
    rpc=Rpc([args.binary],env=dict(os.environ,AUTO_KICAD_STATE_DIR=args.state))
    def initialize():
        rpc.call("initialize",{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"deletion-acceptance","version":"1"}})
        rpc.send({"jsonrpc":"2.0","method":"notifications/initialized"})
    def call(name,parameters,ok=True):
        response=rpc.call("tools/call",{"name":name,"arguments":parameters})
        assert not response.get("isError"),response
        value=response["structuredContent"]
        transcript.append({"tool":name,"response":value})
        assert value["ok"]==ok,value
        return value["data"]
    def inspect(): return call("kicad_inspect_project",{"endpoint":args.endpoint})["pcb"]
    def target(pcb,kind,source):
        if kind=="footprint":
            item=next(f for f in pcb["snapshot"]["native_footprints"]["placements"] if f["reference"]==source)
            source=item["source_uuid"]; identity=item["placement_id"]
        elif kind=="graphic_segment": identity=source
        else:
            item=next(t for t in pcb["snapshot"]["native_routing"]["tracks" if kind=="track" else "vias"] if t["source_uuid"]==source)
            identity=item["track_id" if kind=="track" else "via_id"]
        return dict(kind=kind,target_id=identity,source_uuid=source)
    def plan(pcb,targets,label,ok=True):
        return call("kicad_plan_change",dict(endpoint=args.endpoint,source_run_id="deletion-acceptance",request_id=label,delete_items=dict(observed_revision=pcb["revision"],targets=targets)),ok)
    try:
        initialize()
        original=inspect(); baseline=call("kicad_validate_project",{"endpoint":args.endpoint})
        saved=board.read_bytes()
        protected={board.with_suffix(e):board.with_suffix(e).read_bytes() if board.with_suffix(e).exists() else None for e in (".kicad_pro",".kicad_sch",".kicad_dru")}
        graphic=target(original,"graphic_segment","30000000-0000-4000-8000-000000000005")
        track=target(original,"track","40000000-0000-4000-8000-000000000002")
        via=target(original,"via","50000000-0000-4000-8000-000000000001")
        for label,targets,reason in [
            ("empty",[],"1 to 100"), ("duplicate",[graphic,graphic],"duplicate"),
            ("foreign",[dict(graphic,source_uuid="foreign")],"scope"),
            ("wrong-kind",[dict(track,kind="via")],"scope"),
            ("locked-track",[target(original,"track","40000000-0000-4000-8000-000000000003")],"locked"),
            ("locked-drawing",[target(original,"graphic_segment","30000000-0000-4000-8000-000000000006")],"locked"),
            ("outline",[target(original,"graphic_segment","30000000-0000-4000-8000-000000000001")],"Dwgs.User"),
            ("associated",[target(original,"footprint","J4")],"associated"),
            ("locked-footprint",[target(original,"footprint","J6")],"locked"),
            ("connected-footprint",[target(original,"footprint","J1")],"connected_copper"),
            ("batch-bypass",[track,target(original,"footprint","J1")],"connected_copper"),
        ]:
            rejected=plan(original,targets,label,False)
            assert reason in str(rejected),rejected
            assert board.read_bytes()==saved
        assert plan(dict(original,revision="sha256:"+"0"*64),[graphic],"stale",False)["reason"]=="stale_plan"
        for label,targets in [("required-track",[track]),("required-via",[via])]:
            bad=plan(original,targets,label)
            rejected=call("kicad_preview_change",{k:bad[k] for k in ("plan_id","plan_hash")},False)
            assert rejected["approval_hash"] is None
            assert "disconnected" in rejected["shadow_validation"]["reason"],rejected
            assert rejected["shadow_validation"]["source_unchanged"] and rejected["shadow_validation"]["shadow_cleaned"]
            assert board.read_bytes()==saved
        targets=[graphic,target(original,"track","40000000-0000-4000-8000-000000000004"),target(original,"track","40000000-0000-4000-8000-000000000005"),target(original,"via","50000000-0000-4000-8000-000000000002"),target(original,"footprint","J5")]
        planned=plan(original,targets,"exact-removal")
        params={k:planned[k] for k in ("plan_id","plan_hash")}
        preview=call("kicad_preview_change",params)
        assert preview["shadow_validation"]["normalized_snapshot_matches"] and preview["shadow_validation"]["shadow_cleaned"]
        assert board.read_bytes()==saved
        approval=dict(params,**{k:preview[k] for k in ("approval_hash","validation_evidence_hash")})
        try:
            call("kicad_commit_change",dict(approval,approval_hash="wrong"))
            raise AssertionError("incorrect approval accepted")
        except RuntimeError as error: assert json.loads(str(error))["message"]=="approval mismatch"
        assert board.read_bytes()==saved
        assert call("kicad_commit_change",approval)["state"]=="committed"
        after=inspect(); expected=copy.deepcopy(original["snapshot"])
        removed={t["source_uuid"] for t in targets}
        expected["native_routing"]["tracks"]=[t for t in expected["native_routing"]["tracks"] if t["source_uuid"] not in removed]
        expected["native_routing"]["vias"]=[v for v in expected["native_routing"]["vias"] if v["source_uuid"] not in removed]
        expected["native_footprints"]["placements"]=[f for f in expected["native_footprints"]["placements"] if f["source_uuid"] not in removed]
        expected["board"]["graphics"]=[g for g in expected["board"]["graphics"] if g["graphic_id"] not in removed]
        assert without_revisions(after["snapshot"])==without_revisions(expected),{"actual":after,"expected":expected}
        saved=board.read_bytes()
        final=call("kicad_validate_project",{"endpoint":args.endpoint,"baseline_id":baseline["baseline_id"]})
        assert not final["delta"]["introduced"]
        assert final["report"]["unconnected_items"]<=baseline["report"]["unconnected_items"]
        for restart in [False,True]:
            if restart:
                rpc.close(); rpc=Rpc([args.binary],env=dict(os.environ,AUTO_KICAD_STATE_DIR=args.state)); initialize()
            try:
                call("kicad_commit_change",approval)
                raise AssertionError("deletion replay accepted")
            except RuntimeError as error: assert json.loads(str(error))["code"]==-32602
            assert board.read_bytes()==saved
        assert plan(after,targets,"missing-after-delete",False).get("reason")
        assert all((p.read_bytes() if p.exists() else None)==v for p,v in protected.items())
        Path(args.evidence).write_text(json.dumps(dict(passed=True,before=original["snapshot"],after=inspect()["snapshot"],deleted_targets=targets,baseline_drc=baseline["report"],final_drc=final,restart_replay_rejected=True,transcript=transcript),indent=2)+"\n")
    finally:
        rpc.close()
        if not Path(args.evidence).exists(): Path(args.evidence).write_text(json.dumps(dict(passed=False,transcript=transcript),indent=2)+"\n")

if __name__=="__main__": main()
