//! Complete generated correspondence and identity-only compilation for private DRC copies.
use super::*;

#[derive(Clone, Debug, Serialize)]
pub struct GeneratedZoneCorrespondence {
    identities: Vec<TeardropIdentity>,
}
impl GeneratedZoneCorrespondence {
    pub(crate) fn from_bound(identities: Vec<TeardropIdentity>) -> Self {
        Self { identities }
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
    let mut used = BTreeSet::new();
    authored_ids(
        partition.source.nodes.first().ok_or("missing board root")?,
        &mut used,
    )?;
    let mut by_key = BTreeMap::new();
    for identity in identities {
        if !is_uuid(&identity.native_uuid)
            || !used.insert(identity.native_uuid.clone())
            || by_key
                .insert(
                    identity.correspondence_key.as_str(),
                    identity.native_uuid.as_str(),
                )
                .is_some()
        {
            return Err(
                "DRC generated identity is invalid, duplicated or collides with authored content"
                    .into(),
            );
        }
    }
    if by_key.len() != partition.generated.len() {
        return Err("DRC generated identity set differs".into());
    }
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
    if result.len() > MAX_NATIVE_GEOMETRY_BYTES {
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
}
