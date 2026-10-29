//! Bounded server-owned schematic observations and approved publication sessions.
pub mod protocol;
use crate::{digest_text, library_settings, recovery::ExecutionStore};
use kicad_adapter::schematic_audit::{
    MatchedSchematicPreview, SchematicAudit, SchematicEditPreview,
};
use protocol::{Expectations, GeneralRequest, Net, ObserveRequest, Pin};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const LIVE_LIMIT: usize = 4;
const TERMINAL_LIMIT: usize = 32;
const TTL: Duration = Duration::from_secs(900);

struct Observation {
    audit: SchematicAudit,
    project: PathBuf,
    schematic: PathBuf,
    created: Instant,
}
enum Phase {
    Planned(Arc<SchematicEditPreview>),
    Awaiting {
        preview: Arc<SchematicEditPreview>,
        matched: MatchedSchematicPreview,
        approval: String,
    },
    Busy,
    Terminal(Value),
}
struct Plan {
    source_run: String,
    project: PathBuf,
    hash: String,
    created: Instant,
    phase: Mutex<Phase>,
}
#[derive(Default)]
struct State {
    observations: BTreeMap<String, Arc<Observation>>,
    plans: BTreeMap<String, Arc<Plan>>,
    busy_observations: usize,
    busy_plans: usize,
}
pub(crate) struct Registry {
    state: Mutex<State>,
    sequence: AtomicU64,
    nonce: String,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            sequence: AtomicU64::new(0),
            nonce: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ),
        }
    }
}

fn bounded(value: &impl serde::Serialize) -> Result<(), String> {
    if serde_json::to_vec(value)
        .map_err(|error| error.to_string())?
        .len()
        > 4 * 1024 * 1024
    {
        Err("schematic request exceeds byte bound".into())
    } else {
        Ok(())
    }
}
fn paths(request: &ObserveRequest) -> Result<(PathBuf, PathBuf), String> {
    let project = fs::canonicalize(&request.project_root).map_err(|error| error.to_string())?;
    let schematic = fs::canonicalize(&request.root_schematic).map_err(|error| error.to_string())?;
    if project != Path::new(&request.project_root)
        || schematic != Path::new(&request.root_schematic)
        || !project.is_dir()
        || !schematic.starts_with(&project)
    {
        return Err("schematic paths must be canonical and confined".into());
    }
    Ok((project, schematic))
}
impl State {
    fn prune(&mut self) {
        self.observations
            .retain(|_, value| value.created.elapsed() <= TTL);
        self.plans.retain(|_, value| {
            value.created.elapsed() <= TTL
                || matches!(*value.phase.lock().expect("plan phase"), Phase::Busy)
        });
        let mut terminal = self
            .plans
            .iter()
            .filter(|(_, value)| {
                matches!(*value.phase.lock().expect("plan phase"), Phase::Terminal(_))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        terminal.sort();
        for id in terminal
            .iter()
            .take(terminal.len().saturating_sub(TERMINAL_LIMIT))
        {
            self.plans.remove(id);
        }
    }
    fn live_plans(&self) -> usize {
        self.plans
            .values()
            .filter(|value| !matches!(*value.phase.lock().expect("plan phase"), Phase::Terminal(_)))
            .count()
    }
}
impl Registry {
    pub fn history(
        &self,
        limit: usize,
        source_run: Option<&str>,
        plan_id: Option<&str>,
    ) -> Vec<Value> {
        let mut state = self.state.lock().expect("schematic registry");
        state.prune();
        state.plans.iter().rev().filter(|(id,plan)|source_run.is_none_or(|run|run==plan.source_run)&&plan_id.is_none_or(|expected|expected==id.as_str())).take(limit.min(TERMINAL_LIMIT)).map(|(id,plan)|{
            let phase=plan.phase.lock().expect("plan phase");
            match &*phase {Phase::Terminal(data)=>{let mut data=data.clone();data["plan_id"]=json!(id);data["source_run_id"]=json!(plan.source_run);data["plan_hash"]=json!(plan.hash);data},_=>json!({"plan_id":id,"plan_hash":plan.hash,"source_run_id":plan.source_run,"state":match &*phase {Phase::Planned(_)=>"planned",Phase::Awaiting{..}=>"awaiting_approval",_=>"in_flight"}})}
        }).collect()
    }
    pub fn release(&self, request: protocol::ReleaseRequest) -> Result<Value, String> {
        bounded(&request)?;
        if request.observation_ids.len() + request.plan_ids.len() > 64 {
            return Err("release count exceeds bound".into());
        }
        let mut state = self.state.lock().expect("schematic registry");
        state.prune();
        if request.plan_ids.iter().any(|id| {
            state
                .plans
                .get(id)
                .is_some_and(|plan| matches!(*plan.phase.lock().expect("plan phase"), Phase::Busy))
        }) {
            return Err("cannot release an in-flight schematic operation".into());
        }
        let observations = request
            .observation_ids
            .iter()
            .filter(|id| state.observations.remove(*id).is_some())
            .count();
        let plans = request
            .plan_ids
            .iter()
            .filter(|id| state.plans.remove(*id).is_some())
            .count();
        Ok(json!({"state":"released","observations_released":observations,"plans_released":plans}))
    }
    fn id(&self, prefix: &str) -> String {
        format!(
            "schematic-general-{prefix}:{}:{}",
            self.nonce,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
    pub fn observe(
        &self,
        settings: &library_settings::SharedLibrarySettings,
        request: ObserveRequest,
    ) -> Result<Value, String> {
        bounded(&request)?;
        let (project, schematic) = paths(&request)?;
        {
            let mut state = self.state.lock().expect("schematic registry");
            state.prune();
            if state.observations.len() + state.busy_observations >= LIVE_LIMIT {
                return Err(
                    "schematic observation limit reached; wait for expiry or complete active plans"
                        .into(),
                );
            }
            state.busy_observations += 1;
        }
        let result: Result<(String, Arc<Observation>, Value), String> = (|| {
            let policy = settings
                .as_ref()
                .as_ref()
                .map_err(Clone::clone)?
                .schematic_policy(&project, &schematic)?;
            let audit = SchematicAudit::prepare(&policy)?;
            let connectivity = audit.observe_pin_connectivity()?;
            let id = self.id("observation");
            let units=audit.identities().units().iter().map(|unit|json!({"file":unit.file,"sheet_path":unit.sheet_path,"symbol_uuid":unit.source_uuid,"reference":unit.reference,"unit":unit.unit})).collect::<Vec<_>>();
            let contacts=audit.pins().pins().iter().map(|pin|json!({"pin":Pin::from(&kicad_schematic::probes::ConnectivityPin::Component(pin.key.clone())),"reference":pin.reference,"contacts":pin.occurrences.iter().map(|occurrence|json!({"file":occurrence.source.file,"symbol_uuid":occurrence.source.symbol_uuid,"sheet_path":occurrence.source.sheet_path,"position":{"x_nm":occurrence.position.x.nm(),"y_nm":occurrence.position.y.nm()},"unit":occurrence.unit,"name":occurrence.name,"electrical_type":occurrence.electrical_type})).collect::<Vec<_>>()})).collect::<Vec<_>>();
            let data = json!({"schema_version":1,"observation":"native_schematic","observation_id":id,"revision":audit.revision(),"project_root":project,"root_schematic":schematic,"expires_in_seconds":TTL.as_secs(),"units":units,"contacts":contacts,"objects":audit.edit_inventory()?,"connectivity":{"nets":connectivity.connectivity().nets().iter().map(Net::from).collect::<Vec<_>>(),"isolated_no_connect":connectivity.connectivity().isolated_no_connect().iter().map(Pin::from).collect::<Vec<_>>()},"erc":audit.erc(),"source_context":audit.evidence()?});
            audit.verify()?;
            Ok((
                id,
                Arc::new(Observation {
                    audit,
                    project,
                    schematic,
                    created: Instant::now(),
                }),
                data,
            ))
        })();
        let mut state = self.state.lock().expect("schematic registry");
        state.busy_observations -= 1;
        let (id, observation, data) = result?;
        state.observations.insert(id, observation);
        Ok(data)
    }
    pub fn plan(
        &self,
        store: &ExecutionStore,
        project: &str,
        schematic: &str,
        source_run: &str,
        request_id: &str,
        request: GeneralRequest,
    ) -> Result<Value, String> {
        bounded(&request)?;
        if request.schema_version != 1
            || request.edits.is_empty()
            || request.edits.len() > 128
            || source_run.trim().is_empty()
            || request_id.trim().is_empty()
        {
            return Err(
                "invalid schematic schema version, edit count or run/request identity".into(),
            );
        }
        let observation = {
            let mut state = self.state.lock().expect("schematic registry");
            state.prune();
            if state.live_plans() + state.busy_plans >= LIVE_LIMIT {
                return Err("schematic live plan limit reached".into());
            }
            let observation = state
                .observations
                .get(&request.observation_id)
                .cloned()
                .ok_or("unknown or expired schematic observation")?;
            if observation.project != Path::new(project)
                || observation.schematic != Path::new(schematic)
                || observation.audit.revision() != request.observed_revision
            {
                return Err("schematic observation path or revision mismatch".into());
            }
            state.busy_plans += 1;
            observation
        };
        let result: Result<(String, Arc<Plan>, Value), String> = (|| {
            store
                .check_project_ready(&observation.project)
                .map_err(|error| error.to_string())?;
            observation.audit.verify()?;
            let edits = request
                .edits
                .iter()
                .map(|edit| edit.resolve(&observation.audit))
                .collect::<Result<Vec<_>, _>>()?;
            let preview = observation.audit.preview_edits(&edits)?;
            let id = self.id("plan");
            let operation_digest = digest_text(&[&format!("{edits:?}")]);
            let hash = digest_text(&[
                "general-schematic-v1",
                &self.nonce,
                &id,
                source_run,
                project,
                schematic,
                &request.observed_revision,
                &operation_digest,
                preview.digest(),
            ]);
            let changes=preview.candidate().changes().iter().map(|change|json!({"file":change.file,"uuid":change.uuid,"kind":format!("{:?}",change.kind),"instance_paths":change.instance_paths,"action":format!("{:?}",change.action)})).collect::<Vec<_>>();
            let (before_markers, after_markers) = preview.no_connect_markers()?;
            let markers = |values: Vec<kicad_adapter::schematic_audit::NoConnectMarker>| {
                values.into_iter().map(|marker|json!({"file":marker.file,"uuid":marker.uuid,"position":{"x_nm":marker.position.x.nm(),"y_nm":marker.position.y.nm()},"definition_digest":marker.definition_digest})).collect::<Vec<_>>()
            };
            let data = json!({"schema_version":1,"operation_identity":"general-schematic-v1","plan_id":id,"plan_hash":hash,"state":"planned","changes":changes,"source_artifacts":preview.candidate().source_revision(),"candidate_artifacts":preview.candidate().parsed().revision,"markers_before":markers(before_markers),"markers_after":markers(after_markers),"native_preview":preview.evidence(),"expires_in_seconds":TTL.as_secs()});
            Ok((
                id,
                Arc::new(Plan {
                    source_run: source_run.into(),
                    project: observation.project.clone(),
                    hash,
                    created: Instant::now(),
                    phase: Mutex::new(Phase::Planned(preview)),
                }),
                data,
            ))
        })();
        let mut state = self.state.lock().expect("schematic registry");
        state.busy_plans -= 1;
        let (id, plan, data) = result?;
        state.plans.insert(id, plan);
        Ok(data)
    }
    fn get(&self, id: &str, hash: &str) -> Result<Arc<Plan>, String> {
        let mut state = self.state.lock().expect("schematic registry");
        state.prune();
        let plan = state
            .plans
            .get(id)
            .cloned()
            .ok_or("unknown or expired general schematic plan")?;
        if plan.hash != hash {
            return Err("general schematic plan hash mismatch".into());
        }
        Ok(plan)
    }
    pub fn preview(&self, id: &str, hash: &str, expected: Expectations) -> Result<Value, String> {
        bounded(&expected)?;
        let expected = expected.native()?;
        let plan = self.get(id, hash)?;
        let preview = {
            let mut phase = plan.phase.lock().expect("plan phase");
            if !matches!(*phase, Phase::Planned(_) | Phase::Awaiting { .. }) {
                return Err("schematic plan is consumed, busy or terminal".into());
            }
            match std::mem::replace(&mut *phase, Phase::Busy) {
                Phase::Planned(preview) | Phase::Awaiting { preview, .. } => preview,
                _ => unreachable!(),
            }
        };
        if let Err(error) = preview.verify() {
            *plan.phase.lock().expect("plan phase") =
                Phase::Terminal(json!({"state":"rejected","reason":error}));
            return Err(error);
        }
        let matched = match preview.match_expectations(&expected) {
            Ok(matched) => matched,
            Err(error) => {
                *plan.phase.lock().expect("plan phase") = Phase::Planned(preview);
                return Err(error);
            }
        };
        let approval = digest_text(&[
            "general-schematic-approval-v1",
            &self.nonce,
            id,
            hash,
            matched.digest(),
        ]);
        let data = json!({"schema_version":1,"plan_id":id,"plan_hash":hash,"state":"awaiting_approval","approval_hash":approval,"matched_preview_digest":matched.digest(),"matching":matched.evidence(),"native_preview":preview.evidence()});
        *plan.phase.lock().expect("plan phase") = Phase::Awaiting {
            preview,
            matched,
            approval,
        };
        Ok(data)
    }
    pub fn commit(
        &self,
        store: &ExecutionStore,
        id: &str,
        hash: &str,
        approval: &str,
    ) -> Result<Value, String> {
        let plan = self.get(id, hash)?;
        let matched = {
            let mut phase = plan.phase.lock().expect("plan phase");
            match &*phase {
                Phase::Awaiting {
                    approval: expected, ..
                } if expected == approval => {}
                _ => {
                    return Err(
                        "schematic approval mismatch or plan is not awaiting approval".into(),
                    );
                }
            };
            match std::mem::replace(&mut *phase, Phase::Busy) {
                Phase::Awaiting { matched, .. } => matched,
                _ => unreachable!(),
            }
        };
        let result = (|| {
            let writer = store.acquire(&plan.project).map_err(|error| {
                crate::recovery::SchematicPublicationFailure {
                    unknown: false,
                    attempt_id: None,
                    reason: error.to_string(),
                }
            })?;
            writer.publish_matched_schematic(hash, approval, &matched)
        })();
        let data = match result {
            Ok(receipt) => {
                json!({"schema_version":1,"plan_id":id,"state":"committed","receipt":receipt})
            }
            Err(error) => {
                json!({"schema_version":1,"plan_id":id,"state":if error.unknown{"unknown"}else{"rejected"},"attempt_id":error.attempt_id,"reason":error.reason})
            }
        };
        *plan.phase.lock().expect("plan phase") = Phase::Terminal(data.clone());
        let mut state = self.state.lock().expect("schematic registry");
        state
            .observations
            .retain(|_, observation| observation.project != plan.project);
        state.prune();
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sessions_expire_bound_terminal_metadata_and_refuse_release_while_claimed() {
        let registry = Registry::default();
        {
            let mut state = registry.state.lock().unwrap();
            for index in 0..TERMINAL_LIMIT + 5 {
                state.plans.insert(
                    format!("terminal-{index:03}"),
                    Arc::new(Plan {
                        source_run: "run".into(),
                        project: PathBuf::from("/unused"),
                        hash: "hash".into(),
                        created: Instant::now(),
                        phase: Mutex::new(Phase::Terminal(json!({"state":"committed"}))),
                    }),
                );
            }
            state.plans.insert(
                "expired".into(),
                Arc::new(Plan {
                    source_run: "run".into(),
                    project: PathBuf::from("/unused"),
                    hash: "hash".into(),
                    created: Instant::now() - TTL - Duration::from_secs(1),
                    phase: Mutex::new(Phase::Terminal(json!({"state":"rejected"}))),
                }),
            );
            state.plans.insert(
                "claimed".into(),
                Arc::new(Plan {
                    source_run: "run".into(),
                    project: PathBuf::from("/unused"),
                    hash: "hash".into(),
                    created: Instant::now() - TTL - Duration::from_secs(1),
                    phase: Mutex::new(Phase::Busy),
                }),
            );
            state.prune();
            assert_eq!(state.plans.len(), TERMINAL_LIMIT + 1);
            assert!(!state.plans.contains_key("expired"));
            assert_eq!(state.live_plans(), 1);
        }
        assert!(registry.get("claimed", "wrong").is_err());
        assert!(
            registry
                .release(protocol::ReleaseRequest {
                    observation_ids: vec![],
                    plan_ids: vec!["claimed".into()]
                })
                .is_err()
        );
        assert!(registry.history(100, Some("other"), None).is_empty());
        assert_eq!(registry.history(100, Some("run"), Some("claimed")).len(), 1);
    }
}
