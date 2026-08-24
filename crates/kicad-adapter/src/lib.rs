use kicad_ipc::{OpenDocument, OpenDocumentKind, VersionInfo};
use kicad_model::{
    CapabilityObservation, CapabilityStatus, Diagnostic, DiscoveredDocument,
    DiscoveredDocumentKind, DiscoverySnapshot, ModelError, StableId,
};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentObservation {
    Documents(Vec<OpenDocument>),
    Unsupported(String),
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryInput {
    pub endpoint: String,
    pub version: VersionInfo,
    pub schematic: DocumentObservation,
    pub pcb: DocumentObservation,
}

pub fn discovery_snapshot(input: DiscoveryInput) -> Result<DiscoverySnapshot, ModelError> {
    let mut documents = Vec::new();
    let mut capabilities = vec![CapabilityObservation {
        id: "base.get_version".to_owned(),
        status: CapabilityStatus::Verified,
        detail: None,
    }];
    let mut diagnostics = Vec::new();
    collect_observation(
        "schematic.documents.discover",
        input.schematic,
        &mut documents,
        &mut capabilities,
        &mut diagnostics,
    )?;
    collect_observation(
        "pcb.documents.discover",
        input.pcb,
        &mut documents,
        &mut capabilities,
        &mut diagnostics,
    )?;
    capabilities.push(CapabilityObservation {
        id: "schematic.items.read".to_owned(),
        status: CapabilityStatus::Unknown,
        detail: Some("not probed".to_owned()),
    });
    capabilities.push(CapabilityObservation {
        id: "pcb.items.read".to_owned(),
        status: CapabilityStatus::Unknown,
        detail: Some("not probed".to_owned()),
    });
    diagnostics.push(Diagnostic {
        code: "content_not_observed".to_owned(),
        message: "document discovery does not include schematic or board items".to_owned(),
    });
    Ok(DiscoverySnapshot {
        kicad_version: format!(
            "{}.{}.{}",
            input.version.major, input.version.minor, input.version.patch
        ),
        endpoint: input.endpoint,
        documents,
        capabilities,
        diagnostics,
    }
    .canonical())
}

fn collect_observation(
    capability_id: &str,
    observation: DocumentObservation,
    documents: &mut Vec<DiscoveredDocument>,
    capabilities: &mut Vec<CapabilityObservation>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ModelError> {
    match observation {
        DocumentObservation::Documents(observed) => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Verified,
                detail: None,
            });
            for document in observed {
                match discovered_document(document)? {
                    Some(document) => {
                        if document.project_name.is_none() || document.project_path.is_none() {
                            diagnostics.push(Diagnostic {
                                code: "project_identity_incomplete".to_owned(),
                                message: document.document_id.to_string(),
                            });
                        }
                        documents.push(document);
                    }
                    None => diagnostics.push(Diagnostic {
                        code: "document_identity_unknown".to_owned(),
                        message: capability_id.to_owned(),
                    }),
                }
            }
        }
        DocumentObservation::Unsupported(detail) => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Unsupported,
                detail: Some(detail),
            });
        }
        DocumentObservation::Failed { code, message } => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Unknown,
                detail: Some(code.clone()),
            });
            diagnostics.push(Diagnostic { code, message });
        }
    }
    Ok(())
}

fn push_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&value.len().to_be_bytes());
    output.extend_from_slice(value);
}

fn push_optional(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            push_field(output, value.as_bytes());
        }
        None => output.push(0),
    }
}

fn discovered_document(document: OpenDocument) -> Result<Option<DiscoveredDocument>, ModelError> {
    let kind = match document.kind {
        OpenDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => {
            if filename.is_none() && sheet_path.is_none() && sheet_kiids.is_empty() {
                return Ok(None);
            }
            DiscoveredDocumentKind::Schematic {
                filename,
                sheet_path,
                sheet_kiids,
            }
        }
        OpenDocumentKind::Pcb { board_filename } => {
            if board_filename.is_empty() {
                return Ok(None);
            }
            DiscoveredDocumentKind::Pcb {
                filename: board_filename,
            }
        }
        OpenDocumentKind::Other => return Ok(None),
    };
    let mut identity = Vec::new();
    push_optional(&mut identity, document.project_path.as_deref());
    push_optional(&mut identity, document.project_name.as_deref());
    match &kind {
        DiscoveredDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => {
            push_field(&mut identity, b"schematic");
            push_optional(&mut identity, filename.as_deref());
            push_optional(&mut identity, sheet_path.as_deref());
            for kiid in sheet_kiids {
                push_field(&mut identity, kiid.as_bytes());
            }
        }
        DiscoveredDocumentKind::Pcb { filename } => {
            push_field(&mut identity, b"pcb");
            push_field(&mut identity, filename.as_bytes());
        }
        DiscoveredDocumentKind::Unknown => push_field(&mut identity, b"unknown"),
    }
    let digest = Sha256::digest(&identity);
    Ok(Some(DiscoveredDocument {
        document_id: StableId::new(format!("document:sha256:{}", hex::encode(digest)))?,
        project_name: document.project_name,
        project_path: document.project_path,
        kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version() -> VersionInfo {
        VersionInfo {
            major: 10,
            minor: 0,
            patch: 4,
            full_version: "10.0.4".to_owned(),
        }
    }

    fn schematic(filename: &str) -> OpenDocument {
        OpenDocument {
            project_name: None,
            project_path: None,
            kind: OpenDocumentKind::Schematic {
                filename: Some(filename.to_owned()),
                sheet_path: None,
                sheet_kiids: Vec::new(),
            },
        }
    }

    fn input(documents: Vec<OpenDocument>) -> DiscoveryInput {
        DiscoveryInput {
            endpoint: "ipc:///tmp/kicad/api.sock".to_owned(),
            version: version(),
            schematic: DocumentObservation::Documents(documents),
            pcb: DocumentObservation::Unsupported("no handler".to_owned()),
        }
    }

    #[test]
    fn digest_is_independent_of_document_insertion_order() {
        let first = discovery_snapshot(input(vec![
            schematic("b.kicad_sch"),
            schematic("a.kicad_sch"),
        ]))
        .unwrap();
        let second = discovery_snapshot(input(vec![
            schematic("a.kicad_sch"),
            schematic("b.kicad_sch"),
        ]))
        .unwrap();
        assert_eq!(first.revision().unwrap(), second.revision().unwrap());
    }

    #[test]
    fn missing_project_identity_remains_explicit() {
        let snapshot = discovery_snapshot(input(vec![schematic("xLock.kicad_sch")])).unwrap();
        assert_eq!(snapshot.documents[0].project_name, None);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "project_identity_incomplete" })
        );
    }

    #[test]
    fn unsupported_pcb_and_unprobed_items_are_not_advertised_as_verified() {
        let snapshot = discovery_snapshot(input(vec![schematic("xLock.kicad_sch")])).unwrap();
        let pcb = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "pcb.documents.discover")
            .unwrap();
        let items = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "schematic.items.read")
            .unwrap();
        assert_eq!(pcb.status, CapabilityStatus::Unsupported);
        assert_eq!(items.status, CapabilityStatus::Unknown);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "content_not_observed" })
        );
    }
}
