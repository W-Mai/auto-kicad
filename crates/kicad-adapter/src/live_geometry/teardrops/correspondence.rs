//! Complete generated correspondence and identity-only compilation for private DRC copies.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeneratedZoneCorrespondence {
    identities: Vec<TeardropIdentity>,
}
impl GeneratedZoneCorrespondence {
    pub(crate) fn from_bound(identities: Vec<TeardropIdentity>) -> Self {
        Self { identities }
    }
    /// Complete generated content independent of transient native identities.
    pub fn content_digest(&self) -> Result<String, String> {
        let mut content = self.identities.clone();
        for item in &mut content {
            item.native_uuid.clear();
        }
        Ok(hash_bytes(
            &serde_json::to_vec(&content).map_err(|e| e.to_string())?,
        ))
    }

    /// Mapping requires every generated field to agree. This says nothing about ordinary objects or approval.
    pub fn equivalent_to(&self, other: &Self) -> Result<BTreeMap<String, String>, String> {
        if self.identities.len() != other.identities.len() {
            return Err("generated candidate counts differ".into());
        }
        let mut result = BTreeMap::new();
        let mut targets = BTreeSet::new();
        for (a, b) in self.identities.iter().zip(&other.identities) {
            let mut projected = a.clone();
            projected.native_uuid = b.native_uuid.clone();
            if projected != *b {
                return Err("complete generated candidate content differs".into());
            }
            if result
                .insert(a.native_uuid.clone(), b.native_uuid.clone())
                .is_some()
                || !targets.insert(b.native_uuid.clone())
            {
                return Err("generated candidate mapping is ambiguous".into());
            }
        }
        Ok(result)
    }
}

/// The text is for an owned closed DRC input only; never replace or reload the live source with it.
#[derive(Clone, Debug, Serialize)]
pub struct GeneratedDrcSnapshot {
    input_sha256: String,
    snapshot_sha256: String,
    identities: Vec<TeardropIdentity>,
    #[serde(skip)]
    text: String,
}
impl GeneratedDrcSnapshot {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn input_sha256(&self) -> &str {
        &self.input_sha256
    }
    pub fn snapshot_sha256(&self) -> &str {
        &self.snapshot_sha256
    }
    pub(crate) fn bind(
        saved: &str,
        observed: &TeardropTextPartition,
        native: &NativeBoardGeometry,
    ) -> Result<Self, String> {
        let partition = TeardropTextPartition::parse(saved)?;
        if partition.generated_content()? != observed.generated_content()? {
            return Err("saved generated content differs from bound observation".into());
        }
        let identities = partition.bind(native)?;
        let text = compile(&partition, &identities)?;
        Ok(Self {
            input_sha256: hash_bytes(saved.as_bytes()),
            snapshot_sha256: hash_bytes(text.as_bytes()),
            identities,
            text,
        })
    }
}
/// Identity-only input for closed native DRC, bound to complete native saved content.
/// Source/resource/native-receipt authority remains the caller's responsibility.
#[derive(Clone, Debug, Serialize)]
pub struct ClosedGeneratedDrcSnapshot {
    input_sha256: String,
    native_saved_sha256: String,
    snapshot_sha256: String,
    generated_content: BTreeMap<String, String>,
    private_identities: BTreeMap<String, String>,
    #[serde(skip)]
    text: String,
}
impl ClosedGeneratedDrcSnapshot {
    pub(crate) fn bind_native_readback(source: &str, native_saved: &str) -> Result<Self, String> {
        let partition = TeardropTextPartition::parse_synchronization(source)?;
        let native = TeardropTextPartition::parse_synchronization(native_saved)?;
        let content = partition.generated_content()?;
        if content.is_empty()
            || content != native.generated_content()?
            || super::super::refill_candidate::synchronization_definitions(partition.authored())?
                != super::super::refill_candidate::synchronization_definitions(native.authored())?
            || partition.authored().filled_copper()? != native.authored().filled_copper()?
        {
            return Err(
                "closed generated DRC requires complete matching native saved content".into(),
            );
        }
        let mut reserved = BTreeSet::new();
        authored_ids(
            partition
                .source
                .nodes
                .first()
                .ok_or("missing generated source root")?,
            &mut reserved,
        )?;
        let mut identities = BTreeMap::new();
        for key in content.keys() {
            let mut chosen = None;
            for salt in 0..8192 {
                let digest = hash_bytes(
                    format!("auto-kicad-closed-generated-drc-v1\0{key}\0{salt}").as_bytes(),
                );
                let id = format!(
                    "{}-{}-4{}-a{}-{}",
                    &digest[..8],
                    &digest[8..12],
                    &digest[13..16],
                    &digest[17..20],
                    &digest[20..32]
                );
                if reserved.insert(id.clone()) {
                    chosen = Some(id);
                    break;
                }
            }
            identities.insert(
                key.clone(),
                chosen.ok_or("closed generated identity collision bound exceeded")?,
            );
        }
        let text = compile_mapped(&partition, &identities)?;
        Ok(Self {
            input_sha256: hash_bytes(source.as_bytes()),
            native_saved_sha256: hash_bytes(native_saved.as_bytes()),
            snapshot_sha256: hash_bytes(text.as_bytes()),
            generated_content: content,
            private_identities: identities,
            text,
        })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn input_sha256(&self) -> &str {
        &self.input_sha256
    }
    pub fn snapshot_sha256(&self) -> &str {
        &self.snapshot_sha256
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}
fn authored_ids(node: &Node, ids: &mut BTreeSet<String>) -> Result<(), String> {
    if let Node::List { items: fields, .. } = node {
        if fields
            .first()
            .is_some_and(|n| atom(n).is_ok_and(|s| s == "uuid"))
        {
            ids.insert(scalar(node)?.to_owned());
        }
        for field in fields {
            authored_ids(field, ids)?;
        }
    }
    Ok(())
}
fn compile(
    partition: &TeardropTextPartition,
    identities: &[TeardropIdentity],
) -> Result<String, String> {
    let identities = identities
        .iter()
        .map(|identity| {
            (
                identity.correspondence_key.clone(),
                identity.native_uuid.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mapping = identities.iter().cloned().collect::<BTreeMap<_, _>>();
    if mapping.len() != identities.len() {
        return Err("duplicate generated native identity key".into());
    }
    compile_mapped(partition, &mapping)
}

fn compile_mapped(
    partition: &TeardropTextPartition,
    identities: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut used = BTreeSet::new();
    authored_ids(
        partition.source.nodes.first().ok_or("missing board root")?,
        &mut used,
    )?;
    for id in identities.values() {
        if !is_uuid(id) || !used.insert(id.clone()) {
            return Err(
                "DRC generated identity is invalid, duplicate or collides with authored content"
                    .into(),
            );
        }
    }
    if identities.len() != partition.generated.len() {
        return Err("DRC generated identity set differs".into());
    }
    let by_key = identities;
    let mut insertions = Vec::new();
    for text in partition.generated.values() {
        let id = by_key
            .get(text.correspondence_key.as_str())
            .ok_or("DRC generated identity missing")?;
        insertions.push((
            text.span.end - 1,
            format!(
                " (uuid {})",
                serde_json::to_string(id).map_err(|e| e.to_string())?
            ),
        ));
    }
    insertions.sort_by_key(|v| std::cmp::Reverse(v.0));
    let mut result = partition.raw_text().to_owned();
    for (at, value) in insertions {
        result.insert_str(at, &value);
    }
    if result.len() as u64 > partition.profile.board_bytes() {
        return Err("bound DRC snapshot exceeds observation bound".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    fn fixture() -> (TeardropTextPartition, Vec<TeardropIdentity>) {
        let text = "(kicad_pcb (version 20260206) (future_board \"untouched\") (footprint (uuid nested)) (zone (layer F.Mask) (attr (teardrop (type padvia))) (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1))) (future_zone 42)))";
        let partition = TeardropTextPartition::parse(text).unwrap();
        let identity = TeardropIdentity {
            native_uuid: ID.into(),
            correspondence_key: partition
                .generated
                .values()
                .next()
                .unwrap()
                .correspondence_key
                .clone(),
            kind: "padvia".into(),
            layer: 42,
            net: "".into(),
            text_definition_digest: "a".into(),
            text_content_digest: "b".into(),
            native_definition_digest: "c".into(),
            native_content_digest: "d".into(),
        };
        (partition, vec![identity])
    }
    #[test]
    fn only_complete_generated_correspondence_grants_a_bijection() {
        let (_, ids) = fixture();
        let a = GeneratedZoneCorrespondence::from_bound(ids.clone());
        let mut changed = ids.clone();
        changed[0].native_uuid = "bbbbbbbb-0000-4000-8000-000000000001".into();
        let b = GeneratedZoneCorrespondence::from_bound(changed.clone());
        assert_eq!(a.equivalent_to(&b).unwrap()[ID], changed[0].native_uuid);
        assert_eq!(a.content_digest().unwrap(), b.content_digest().unwrap());
        for field in 0..8 {
            let mut changed = changed.clone();
            match field {
                0 => changed[0].correspondence_key.push('x'),
                1 => changed[0].text_definition_digest.push('x'),
                2 => changed[0].text_content_digest.push('x'),
                3 => changed[0].native_definition_digest.push('x'),
                4 => changed[0].native_content_digest.push('x'),
                5 => changed[0].net.push('x'),
                6 => changed[0].layer += 1,
                _ => changed[0].kind.push('x'),
            }
            assert!(
                a.equivalent_to(&GeneratedZoneCorrespondence::from_bound(changed))
                    .is_err()
            );
        }
        assert!(
            a.equivalent_to(&GeneratedZoneCorrespondence::from_bound(vec![]))
                .is_err()
        );
        let duplicate =
            GeneratedZoneCorrespondence::from_bound(vec![ids[0].clone(), ids[0].clone()]);
        assert!(duplicate.equivalent_to(&duplicate).is_err());
    }
    #[test]
    fn private_drc_compilation_changes_only_bound_generated_uuid_insertions() {
        let (partition, ids) = fixture();
        let compiled = compile(&partition, &ids).unwrap();
        assert_eq!(
            compiled.replace(&format!(" (uuid \"{ID}\")"), ""),
            partition.raw_text()
        );
        for id in [
            "nested",
            "not-a-uuid",
            "aaaaaaaa-0000-4000-8000-00000000000A",
        ] {
            let mut wrong = ids.clone();
            wrong[0].native_uuid = id.into();
            assert!(compile(&partition, &wrong).is_err());
        }
        let colliding =
            TeardropTextPartition::parse(&partition.raw_text().replace("nested", ID)).unwrap();
        assert!(compile(&colliding, &ids).is_err());
        assert!(compile(&partition, &[]).is_err());
        let mut wrong = ids;
        wrong[0].correspondence_key.push('x');
        assert!(compile(&partition, &wrong).is_err());
    }
    #[test]
    fn closed_generated_drc_requires_complete_native_content_and_stable_private_identity() {
        let text = r#"(kicad_pcb (version 20260206) (future_board "untouched") (zone (layer F.Mask) (attr (teardrop (type padvia))) (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1))) (future_zone 42)))"#;
        let first = ClosedGeneratedDrcSnapshot::bind_native_readback(text, text).unwrap();
        let second = ClosedGeneratedDrcSnapshot::bind_native_readback(text, text).unwrap();
        assert_eq!(first.text(), second.text());
        assert_eq!(first.private_identities.len(), 1);
        let id = first.private_identities.values().next().unwrap();
        assert!(is_uuid(id));
        assert_eq!(first.text().replace(&format!(" (uuid \"{id}\")"), ""), text);
        for changed in [
            text.replace("untouched", "changed"),
            text.replace("future_zone 42", "future_zone 43"),
            text.replace("xy 1 0", "xy 2 0"),
        ] {
            assert!(ClosedGeneratedDrcSnapshot::bind_native_readback(text, &changed).is_err());
        }
    }
}
