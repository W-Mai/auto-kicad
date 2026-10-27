//! Live and closed native agreement for complete, saved electrical components.
use crate::{geometry::GeometryObservation, live_geometry::TeardropTextPartition};
use kicad_checks::{
    DrcPolicy,
    connectivity::{ClosedConnectivityEvidence, check_saved_connectivity},
};
use kicad_ipc::{DocumentQuery, InstanceToken, KiCadClient, KiCadTransport, OpenDocumentKind};
use kicad_model::RevisionDigest;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct DocumentBinding {
    project_name: Option<String>,
    project_path: Option<String>,
    filename: String,
}
impl DocumentBinding {
    fn capture<T: KiCadTransport>(
        client: &mut KiCadClient<T>,
        filename: &str,
    ) -> Result<Self, String> {
        let documents = client
            .get_open_documents(DocumentQuery::Pcb)
            .map_err(|e| e.to_string())?;
        if documents.len() != 1 {
            return Err("connectivity requires one exact open PCB document".into());
        }
        let document = &documents[0];
        if !matches!(&document.kind, OpenDocumentKind::Pcb { board_filename } if board_filename == filename)
        {
            return Err("connectivity document does not match".into());
        }
        Ok(Self {
            project_name: document.project_name.clone(),
            project_path: document.project_path.clone(),
            filename: filename.into(),
        })
    }
}

/// Constructed after repeated complete observations and per-endpoint native agreement.
#[derive(Clone, Debug, Serialize)]
pub struct VerifiedCopperConnectivity {
    #[serde(skip)]
    instance: InstanceToken,
    instance_identity: String,
    document: DocumentBinding,
    board_path: PathBuf,
    filename: String,
    revision: RevisionDigest,
    closed: ClosedConnectivityEvidence,
}

impl VerifiedCopperConnectivity {
    pub fn closed(&self) -> &ClosedConnectivityEvidence {
        &self.closed
    }
    pub fn revision(&self) -> &RevisionDigest {
        &self.revision
    }

    pub fn capture<T: KiCadTransport>(
        client: &mut KiCadClient<T>,
        filename: &str,
        observed: &GeometryObservation,
        saved: &str,
        board_path: &Path,
        policy: &DrcPolicy,
    ) -> Result<Self, String> {
        let deadline = Instant::now() + policy.timeout.min(Duration::from_secs(60));
        let complete = observed
            .generated()
            .ok_or("connectivity requires complete generated geometry")?;
        let instance = client
            .instance_token()
            .cloned()
            .ok_or("unbound connectivity editor")?;
        observed.verify_current(client, filename)?;
        let document = DocumentBinding::capture(client, filename)?;
        let canonical_board = std::fs::canonicalize(board_path).map_err(|e| e.to_string())?;
        let project_path = document
            .project_path
            .as_deref()
            .ok_or("missing connectivity project path")?;
        let project_path = std::fs::canonicalize(project_path).map_err(|e| e.to_string())?;
        if canonical_board.parent() != Some(project_path.as_path())
            || canonical_board.file_name() != Path::new(filename).file_name()
        {
            return Err("closed connectivity path differs from the live document".into());
        }

        if client
            .get_version()
            .map_err(|e| e.to_string())?
            .full_version
            != "10.0.7"
        {
            return Err("unverified connectivity native version".into());
        }
        let saved_partition = TeardropTextPartition::parse(saved)?;
        complete
            .text()
            .authored()
            .verify_saved_geometry(saved_partition.authored())?;
        // Bind all generated content and identities, including regions absent from ordinary IPC zones.
        let _ = complete.drc_snapshot(saved)?;
        let closed = check_saved_connectivity(
            board_path,
            &DrcPolicy {
                allowed_root: policy.allowed_root.clone(),
                timeout: deadline.saturating_duration_since(Instant::now()),
            },
        )
        .map_err(|e| format!("closed connectivity: {e:?}"))?;
        if closed.board_sha256() != hex::encode(Sha256::digest(saved.as_bytes())) {
            return Err("connectivity saved text differs from closed native input".into());
        }
        for _ in 0..2 {
            for endpoint in closed.endpoints() {
                if Instant::now() >= deadline {
                    return Err("live connectivity deadline exceeded".into());
                }
                let actual = client
                    .observed_copper_component(filename, endpoint.uuid(), endpoint.net())
                    .map_err(|e| e.to_string())?;
                let expected = closed
                    .component(endpoint.uuid())
                    .ok_or("missing closed component")?
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                // KiCad omits netless items from its connectivity graph. Existence is already
                // proven by complete live/saved content and the independent native endpoint set.
                let matches = match actual {
                    Some(actual) => actual == expected,
                    None => {
                        endpoint.net().is_empty()
                            && expected == BTreeSet::from([endpoint.uuid().to_owned()])
                    }
                };
                if !matches {
                    return Err("live and closed native physical components differ".into());
                }
            }
            observed.verify_current(client, filename)?;
            if DocumentBinding::capture(client, filename)? != document
                || client.instance_token() != Some(&instance)
            {
                return Err("connectivity instance changed".into());
            }
        }
        closed
            .verify_saved_inputs(board_path, policy)
            .map_err(|e| format!("connectivity input binding: {e:?}"))?;
        if Instant::now() >= deadline {
            return Err("live connectivity deadline exceeded".into());
        }
        Ok(Self {
            instance_identity: instance.as_str().into(),
            document,
            board_path: canonical_board,
            instance,
            filename: filename.into(),
            revision: observed.context().revision.clone(),
            closed,
        })
    }

    /// Revoke reuse after any editor, complete revision, document or saved-input change.
    pub fn verify_current<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
        observed: &GeometryObservation,
        board: &Path,
        policy: &DrcPolicy,
    ) -> Result<(), String> {
        if std::fs::canonicalize(board).map_err(|e| e.to_string())? != self.board_path {
            return Err("connectivity saved path changed".into());
        }
        if client.instance_token() != Some(&self.instance)
            || filename != self.filename
            || observed.context().revision != self.revision
        {
            return Err("connectivity instance, document or revision changed".into());
        }
        observed.verify_current(client, filename)?;
        if DocumentBinding::capture(client, filename)? != self.document
            || client.instance_token() != Some(&self.instance)
        {
            return Err("connectivity document or instance changed".into());
        }
        self.closed
            .verify_saved_inputs(board, policy)
            .map_err(|e| format!("connectivity input binding: {e:?}"))
    }
}
