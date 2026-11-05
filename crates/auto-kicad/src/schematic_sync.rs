//! Closed-document synchronization sessions; file publication is owned by guarded recovery.
use crate::{digest_text, library_settings, recovery::ExecutionStore};
use kicad_adapter::schematic_audit::{
    SchematicAudit,
    synchronization::{
        MatchedSynchronizationPreview, SynchronizationBoardObservation,
        SynchronizationBoardPreview, SynchronizationExpectation, board::SynchronizationIntent,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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

mod preview;
pub use preview::Expectation;
use preview::{Matched, Preview};

fn is_false(value: &bool) -> bool {
    !*value
}

const LIVE_LIMIT: usize = 4;
const TERMINAL_LIMIT: usize = 32;
const TTL: Duration = Duration::from_secs(900);
const PREFIX: &str = "schematic-pcb-sync-plan:";

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Inspection {
    Observe {
        project_root: String,
        root_schematic: String,
        #[serde(default, skip_serializing_if = "is_false")]
        project_netclasses: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        regions: bool,
    },
    Release {
        #[serde(default)]
        observation_ids: Vec<String>,
        #[serde(default)]
        plan_ids: Vec<String>,
    },
    History {
        #[serde(default)]
        limit: Option<usize>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub observation_id: String,
    pub observed_revision: String,
    pub intent: SynchronizationIntent,
}
struct Observation {
    observed: Arc<SynchronizationBoardObservation>,
    project: PathBuf,
    created: Instant,
}
enum Phase {
    Planned(Preview),
    Awaiting {
        matched: Box<Matched>,
        approval: String,
        validation: String,
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
fn bounded(value: &impl Serialize) -> Result<(), String> {
    if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
        return Err("synchronization request/response exceeds 4 MiB bound".into());
    }
    Ok(())
}
pub(crate) fn is_plan(id: &str) -> bool {
    id.starts_with(PREFIX)
}
impl State {
    fn prune(&mut self) {
        self.observations.retain(|_, o| o.created.elapsed() <= TTL);
        self.plans.retain(|_, p| {
            p.created.elapsed() <= TTL
                || matches!(*p.phase.lock().expect("sync phase"), Phase::Busy)
        });
        let terminal = self
            .plans
            .iter()
            .filter(|(_, p)| matches!(*p.phase.lock().expect("sync phase"), Phase::Terminal(_)))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
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
            .filter(|p| !matches!(*p.phase.lock().expect("sync phase"), Phase::Terminal(_)))
            .count()
    }
}
impl Registry {
    fn id(&self, kind: &str) -> String {
        format!(
            "schematic-pcb-sync-{kind}:{}:{}",
            self.nonce,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
    pub fn inspect(
        &self,
        settings: &library_settings::SharedLibrarySettings,
        request: Inspection,
    ) -> Result<Value, String> {
        bounded(&request)?;
        match request {
            Inspection::Observe {
                project_root,
                root_schematic,
                project_netclasses,
                regions,
            } => self.observe(
                settings,
                &project_root,
                &root_schematic,
                project_netclasses,
                regions,
            ),
            Inspection::Release {
                observation_ids,
                plan_ids,
            } => self.release(&observation_ids, &plan_ids),
            Inspection::History { limit } => self.history(limit.unwrap_or(TERMINAL_LIMIT)),
        }
    }
    fn observe(
        &self,
        settings: &library_settings::SharedLibrarySettings,
        project: &str,
        schematic: &str,
        project_netclasses: bool,
        regions: bool,
    ) -> Result<Value, String> {
        let root = fs::canonicalize(project).map_err(|e| e.to_string())?;
        let source = fs::canonicalize(schematic).map_err(|e| e.to_string())?;
        if root != Path::new(project)
            || source != Path::new(schematic)
            || !root.is_dir()
            || !source.starts_with(&root)
        {
            return Err("synchronization source paths must be canonical and confined".into());
        }
        {
            let mut state = self.state.lock().expect("sync registry");
            state.prune();
            if state.observations.len() + state.busy_observations >= LIVE_LIMIT {
                return Err(
                    "synchronization observation limit reached; release or wait for expiry".into(),
                );
            }
            state.busy_observations += 1;
        }
        let result = (|| -> Result<_, String> {
            let policy = settings
                .as_ref()
                .as_ref()
                .map_err(Clone::clone)?
                .schematic_policy(&root, &source)?;
            let audit = SchematicAudit::prepare(&policy)?;
            let synchronization = audit.synchronization_source()?;
            let observed = if regions {
                synchronization.observe_region_board(&source.with_extension("kicad_pcb"))?
            } else {
                synchronization.observe_board(&source.with_extension("kicad_pcb"))?
            };
            let id = self.id("observation");
            let mut data = json!({"schema_version":1,"observation":"closed_schematic_pcb_sync","observation_id":id,
                "revision":observed.digest(),"project_root":root,"root_schematic":source,"board":observed.path(),
                "components":synchronization.components(),"ineligible_units":synchronization.ineligible_units(),
                "footprints":observed.board().footprints(),"copper":observed.board().routes().iter().map(|(uuid,(net,locked))|json!({"uuid":uuid,"net":net,"locked":locked})).collect::<Vec<_>>(),"has_zones":observed.board().has_zones(),"source_context":audit.evidence()?,"expires_in_seconds":TTL.as_secs()});
            if let Some(regions) = observed.region_evidence()? {
                data["zones"] = json!(observed.board().zones()?);
                data["region_ownership"] = regions;
            }
            if project_netclasses {
                let classes = observed.observe_project_netclasses()?;
                data["project_netclasses"] = json!({"digest":classes.digest()?,"project_sha256":classes.project_sha256(),
                    "board_sha256":classes.board_sha256(),"configured":classes.configured(),"effective":classes.effective(),"selections":observed.project_netclass_selections()?});
            }
            bounded(&data)?;
            observed.verify()?;
            Ok((
                id,
                Arc::new(Observation {
                    observed,
                    project: root,
                    created: Instant::now(),
                }),
                data,
            ))
        })();
        let mut state = self.state.lock().expect("sync registry");
        state.busy_observations -= 1;
        let (id, observed, data) = result?;
        state.observations.insert(id, observed);
        Ok(data)
    }
    fn release(&self, observations: &[String], plans: &[String]) -> Result<Value, String> {
        if observations.len() + plans.len() > 64 {
            return Err("synchronization release count exceeds bound".into());
        }
        let mut state = self.state.lock().expect("sync registry");
        state.prune();
        if plans.iter().any(|id| {
            state
                .plans
                .get(id)
                .is_some_and(|p| matches!(*p.phase.lock().expect("sync phase"), Phase::Busy))
        }) {
            return Err("cannot release an in-flight synchronization operation".into());
        }
        let removed_observations = observations
            .iter()
            .filter(|id| state.observations.remove(*id).is_some())
            .count();
        let removed_plans = plans
            .iter()
            .filter(|id| state.plans.remove(*id).is_some())
            .count();
        Ok(
            json!({"state":"released","observations_released":removed_observations,"plans_released":removed_plans}),
        )
    }
    fn history(&self, limit: usize) -> Result<Value, String> {
        let mut state = self.state.lock().expect("sync registry");
        state.prune();
        let rows=state.plans.iter().rev().take(limit.min(TERMINAL_LIMIT)).map(|(id,p)|{
            let phase=p.phase.lock().expect("sync phase");
            let mut data=match &*phase {Phase::Terminal(data)=>data.clone(),_=>json!({"state":match &*phase {Phase::Planned(_)=>"planned",Phase::Awaiting{..}=>"awaiting_approval",_=>"in_flight"}})};
            data["plan_id"]=json!(id);data["plan_hash"]=json!(p.hash);data["source_run_id"]=json!(p.source_run);data
        }).collect::<Vec<_>>();
        let data = json!({"observation":"synchronization_session_history","plans":rows});
        bounded(&data)?;
        Ok(data)
    }
    pub fn plan(
        &self,
        store: &ExecutionStore,
        source_run: &str,
        request_id: &str,
        request: Request,
    ) -> Result<Value, String> {
        bounded(&request)?;
        if request.schema_version != 1
            || request.intent.schema_version != 1
            || source_run.trim().is_empty()
            || request_id.trim().is_empty()
            || source_run.len() > 4096
            || request_id.len() > 4096
            || (request.intent.actions.is_empty() && request.intent.project_netclasses.is_empty())
            || request.intent.project_netclasses.len() > 128
            || request.intent.actions.len() > 128
            || request.intent.copper_nets.len() > 4096
            || request.intent.zone_nets.len() > 256
        {
            return Err("invalid synchronization schema, identity or action bound".into());
        }
        let observation = {
            let mut state = self.state.lock().expect("sync registry");
            state.prune();
            if state.live_plans() + state.busy_plans >= LIVE_LIMIT {
                return Err("synchronization live plan limit reached".into());
            }
            let observed = state
                .observations
                .get(&request.observation_id)
                .cloned()
                .ok_or("unknown or expired synchronization observation")?;
            if observed.observed.digest() != request.observed_revision {
                return Err("synchronization observation revision mismatch".into());
            }
            state.busy_plans += 1;
            observed
        };
        let result = (|| -> Result<_, String> {
            store
                .check_project_ready(&observation.project)
                .map_err(|e| e.to_string())?;
            observation.observed.verify()?;
            let preview = Preview::from(observation.observed.preview_intent(&request.intent)?);
            let id = self.id("plan");
            let intent = serde_json::to_string(&request.intent).map_err(|e| e.to_string())?;
            let hash = digest_text(&[
                "schematic-pcb-sync-v1",
                &self.nonce,
                &id,
                source_run,
                request_id,
                &request.observed_revision,
                &intent,
                preview.digest(),
            ]);
            let has_changes = preview.has_changes();
            let mut data = json!({"schema_version":1,"operation_identity":"schematic-pcb-sync-v1","plan_id":id,"plan_hash":hash,
                "state":if has_changes{"planned"}else{"unchanged"},"has_changes":has_changes,"differences":preview.differences(),
                "drc_delta":preview.drc(),"native_preview":preview.evidence(),"expires_in_seconds":TTL.as_secs()});
            if let Some(regions) = preview.regions() {
                data["regions"] = json!(regions);
            }
            if !request.intent.project_netclasses.is_empty() {
                if let Some(project) = preview.project_data() {
                    data["publication_kind"] = json!("project_and_optional_board");
                    for (key, value) in
                        project.as_object().ok_or("invalid project planning data")?
                    {
                        data[key] = value.clone();
                    }
                } else {
                    data["publication_kind"] = json!("board_only");
                }
            }
            bounded(&data)?;
            let phase = if has_changes {
                Phase::Planned(preview)
            } else {
                Phase::Terminal(data.clone())
            };
            Ok((
                id,
                Arc::new(Plan {
                    source_run: source_run.into(),
                    project: observation.project.clone(),
                    hash,
                    created: Instant::now(),
                    phase: Mutex::new(phase),
                }),
                data,
            ))
        })();
        let mut state = self.state.lock().expect("sync registry");
        state.busy_plans -= 1;
        let (id, plan, data) = result?;
        state.plans.insert(id, plan);
        state.prune();
        Ok(data)
    }
    fn get(&self, id: &str, hash: &str) -> Result<Arc<Plan>, String> {
        let mut state = self.state.lock().expect("sync registry");
        state.prune();
        let plan = state
            .plans
            .get(id)
            .cloned()
            .ok_or("unknown or expired synchronization plan")?;
        if plan.hash != hash {
            return Err("synchronization plan hash mismatch".into());
        }
        Ok(plan)
    }
    pub fn preview(&self, id: &str, hash: &str, expectation: Expectation) -> Result<Value, String> {
        bounded(&expectation)?;
        let plan = self.get(id, hash)?;
        let preview = {
            let mut phase = plan.phase.lock().expect("sync phase");
            if !matches!(*phase, Phase::Planned(_) | Phase::Awaiting { .. }) {
                return Err("synchronization plan is consumed, busy or terminal".into());
            }
            match std::mem::replace(&mut *phase, Phase::Busy) {
                Phase::Planned(p) => p,
                Phase::Awaiting { matched, .. } => matched.preview(),
                _ => unreachable!(),
            }
        };
        if let Err(reason) = preview.verify() {
            *plan.phase.lock().expect("sync phase") =
                Phase::Terminal(json!({"state":"rejected","reason":reason}));
            return Err(reason);
        }
        let matched = match preview.match_expectation(expectation) {
            Ok(m) => m,
            Err(e) => {
                *plan.phase.lock().expect("sync phase") = Phase::Planned(preview);
                return Err(e);
            }
        };
        let validation = digest_text(&[
            "schematic-pcb-sync-validation-v1",
            &self.nonce,
            id,
            hash,
            matched.digest(),
        ]);
        let approval = digest_text(&[
            "schematic-pcb-sync-approval-v1",
            &self.nonce,
            id,
            hash,
            &validation,
            matched.digest(),
        ]);
        let data = json!({"schema_version":1,"plan_id":id,"plan_hash":hash,"state":"awaiting_approval","approval_hash":approval,
            "validation_evidence_hash":validation,"matched_preview_digest":matched.digest(),"expectation":matched.expectation()});
        if let Err(e) = bounded(&data) {
            *plan.phase.lock().expect("sync phase") = Phase::Planned(preview);
            return Err(e);
        }
        *plan.phase.lock().expect("sync phase") = Phase::Awaiting {
            matched: Box::new(matched),
            approval,
            validation,
        };
        Ok(data)
    }
    pub fn commit(
        &self,
        store: &ExecutionStore,
        id: &str,
        hash: &str,
        approval: &str,
        validation: &str,
    ) -> Result<Value, String> {
        let plan = self.get(id, hash)?;
        let matched = {
            let mut phase = plan.phase.lock().expect("sync phase");
            match &*phase {
                Phase::Awaiting {
                    approval: expected,
                    validation: evidence,
                    ..
                } if expected == approval && evidence == validation => {}
                _ => return Err(
                    "synchronization approval/evidence mismatch or plan is not awaiting approval"
                        .into(),
                ),
            }
            match std::mem::replace(&mut *phase, Phase::Busy) {
                Phase::Awaiting { matched, .. } => matched,
                _ => unreachable!(),
            }
        };
        let result = store
            .acquire(&plan.project)
            .map_err(|e| crate::recovery::SynchronizationPublicationFailure {
                unknown: false,
                attempt_id: None,
                reason: e.to_string(),
            })
            .and_then(|writer| (*matched).publish(&writer, hash, approval));
        let data = match result {
            Ok(receipt) => {
                json!({"schema_version":1,"plan_id":id,"state":"committed","receipt":receipt})
            }
            Err(e) => {
                json!({"schema_version":1,"plan_id":id,"state":if e.unknown{"unknown"}else{"rejected"},"attempt_id":e.attempt_id,"reason":e.reason})
            }
        };
        *plan.phase.lock().expect("sync phase") = Phase::Terminal(data.clone());
        let mut state = self.state.lock().expect("sync registry");
        state.observations.retain(|_, o| o.project != plan.project);
        state.prune();
        Ok(data)
    }
}

pub(crate) fn recovery_policy(
    settings: &library_settings::SharedLibrarySettings,
    store: &ExecutionStore,
    project: &Path,
    identity: &str,
    execution: bool,
) -> Result<Option<kicad_adapter::schematic_audit::SchematicAuditPolicy>, String> {
    let source = if execution {
        store.synchronization_schematic_for_attempt(project, identity)
    } else {
        store.synchronization_schematic_for_recovery(project, identity)
    }
    .map_err(|e| e.to_string())?;
    source
        .map(|source| {
            settings
                .as_ref()
                .as_ref()
                .map_err(Clone::clone)?
                .schematic_policy(project, &source)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synchronization_session_bounds_expiry_and_busy_release_are_enforced() {
        let registry = Registry::default();
        {
            let mut state = registry.state.lock().unwrap();
            for index in 0..TERMINAL_LIMIT + 5 {
                state.plans.insert(
                    format!("terminal-{index:03}"),
                    Arc::new(Plan {
                        source_run: "run".into(),
                        project: "/unused".into(),
                        hash: "hash".into(),
                        created: Instant::now(),
                        phase: Mutex::new(Phase::Terminal(json!({"state":"committed"}))),
                    }),
                );
            }
            state.plans.insert(
                "busy".into(),
                Arc::new(Plan {
                    source_run: "run".into(),
                    project: "/unused".into(),
                    hash: "hash".into(),
                    created: Instant::now() - TTL - Duration::from_secs(1),
                    phase: Mutex::new(Phase::Busy),
                }),
            );
            state.prune();
            assert_eq!(state.plans.len(), TERMINAL_LIMIT + 1);
            assert_eq!(state.live_plans(), 1);
        }
        assert!(registry.release(&[], &["busy".into()]).is_err());
        assert!(registry.get("busy", "wrong").is_err());
        assert!(is_plan("schematic-pcb-sync-plan:any"));
        assert!(!is_plan("schematic-general-plan:any"));
    }

    #[test]
    fn public_sync_schema_is_typed_and_closes_every_action_shape() {
        let schema = serde_json::to_value(schemars::schema_for!(Request)).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        let definitions = schema["$defs"].as_object().unwrap();
        let actions = definitions["SynchronizationAction"]["oneOf"]
            .as_array()
            .unwrap();
        assert_eq!(actions.len(), 8);
        for action in actions {
            assert_eq!(action["additionalProperties"], false);
            assert!(action["properties"]["kind"]["const"].is_string());
        }
        for name in ["SynchronizationIntent", "SynchronizationPlacement"] {
            assert_eq!(definitions[name]["additionalProperties"], false);
        }
        let expected =
            serde_json::to_value(schemars::schema_for!(SynchronizationExpectation)).unwrap();
        assert_eq!(expected["additionalProperties"], false);
        for name in [
            "SynchronizationDifference",
            "CompleteDrcExpectation",
            "CompleteDrcDiagnostic",
        ] {
            assert_eq!(expected["$defs"][name]["additionalProperties"], false);
        }
    }
}
