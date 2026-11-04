//! Complete source component authority for saved-board synchronization.
use super::*;
use kicad_schematic::{identities::ComponentIdentity, pins::SourceUnitKey};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
pub mod board;
mod drc;
mod native;
mod observation;
pub mod project;
mod templates;
pub use observation::{
    MatchedSynchronizationPreview, MatchedSynchronizationProjectBoardPreview,
    MatchedSynchronizationProjectPreview, NetclassRecordChange,
    PreparedSynchronizationProjectBoardPublication, PreparedSynchronizationProjectPublication,
    PreparedSynchronizationPublication, ProjectNetclassDelta, SourceNetclass,
    SynchronizationBoardObservation, SynchronizationBoardPreview, SynchronizationExpectation,
    SynchronizationIntentPreview, SynchronizationProjectBoardExpectation,
    SynchronizationProjectBoardPreview, SynchronizationProjectExpectation,
    SynchronizationProjectPreview, SynchronizationProjectRecoveryBaseline,
    SynchronizationProjectWrite, SynchronizationRecoveryBaseline,
    VerifiedSynchronizationProjectBoardPublication, VerifiedSynchronizationProjectPublication,
    VerifiedSynchronizationProjectRecovery, VerifiedSynchronizationPublication,
    VerifiedSynchronizationRestoration,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynchronizationUnit {
    pub name: String,
    pub pins: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynchronizationNet {
    pub name: String,
    pub class: Option<String>,
    pub electrical_no_connect: bool,
    pub explicit_no_connect: bool,
    pub pin_function: String,
    pub pin_type: String,
}

/// Derived only from complete captured source units, native netlist and pin evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynchronizationComponent {
    id: String,
    reference: String,
    value: String,
    footprint: Option<String>,
    aliases: Vec<String>,
    canonical_path: String,
    sheet_name: String,
    sheet_file: String,
    units: Vec<SynchronizationUnit>,
    placed_units: BTreeSet<u32>,
    pins: BTreeMap<String, SynchronizationNet>,
    in_bom: bool,
    on_board: bool,
    dnp: bool,
    fields: BTreeMap<String, String>,
    conflicting_fields: BTreeSet<String>,
}
impl SynchronizationComponent {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn reference(&self) -> &str {
        &self.reference
    }
    pub fn footprint(&self) -> Option<&str> {
        self.footprint.as_deref()
    }
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }
    pub fn eligible_for_board(&self) -> bool {
        self.on_board && !self.reference.starts_with('#')
    }
    pub fn pins(&self) -> &BTreeMap<String, SynchronizationNet> {
        &self.pins
    }
}

/// Complete source units without eligible native board-pin authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynchronizationIneligibleUnit {
    pub file: String,
    pub sheet_path: String,
    pub source_uuid: String,
    pub reference: String,
    pub unit: u32,
    pub alias: String,
    pub native_exported: bool,
    pub on_board: bool,
}

/// Captured source authority and shared permanent drift invalidation, not write approval.
#[derive(Clone)]
pub struct SynchronizationSource {
    audit: SchematicAudit,
    connectivity: Arc<SchematicConnectivityAudit>,
    components: Arc<[SynchronizationComponent]>,
    ineligible_units: Arc<[SynchronizationIneligibleUnit]>,
    valid: Arc<AtomicBool>,
    digest: String,
}
impl SynchronizationSource {
    pub fn components(&self) -> &[SynchronizationComponent] {
        &self.components
    }
    pub fn ineligible_units(&self) -> &[SynchronizationIneligibleUnit] {
        &self.ineligible_units
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn audit(&self) -> &SchematicAudit {
        &self.audit
    }
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("synchronization source was invalidated".into());
        }
        if let Err(error) = self.audit.verify() {
            self.valid.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }
    pub fn evidence(&self) -> Result<Value, String> {
        self.verify()?;
        Ok(
            serde_json::json!({"digest":self.digest,"audit":self.audit.evidence()?,
            "connectivity":self.connectivity.evidence(),"components":self.components.as_ref(),"ineligible_units":self.ineligible_units.as_ref()}),
        )
    }
}

impl SchematicAudit {
    /// Bind every exported component to all source units and complete logical pad-net authority.
    pub fn synchronization_source(&self) -> Result<SynchronizationSource, String> {
        self.verify()?;
        let connectivity = Arc::new(self.observe_pin_connectivity()?);
        let mut components = Vec::new();
        let index = SourceIndex::new(self)?;
        let mut metadata_bytes = 0usize;
        let mut occupied_aliases = BTreeSet::new();
        for identity in self.identities().components() {
            let component = source_component(identity, &index)?;
            metadata_bytes = metadata_bytes
                .checked_add(
                    serde_json::to_vec(&component)
                        .map_err(|e| e.to_string())?
                        .len(),
                )
                .ok_or("source component metadata size overflow")?;
            if metadata_bytes > 128 * 1024 * 1024 {
                return Err("expanded source component metadata exceeds bound".into());
            }
            for alias in &component.aliases {
                if !occupied_aliases.insert(alias.clone()) {
                    return Err("source component aliases overlap".into());
                }
            }
            components.push(component);
        }
        components.sort_by(|a, b| a.id.cmp(&b.id));
        if components.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err("duplicate synchronization component identity".into());
        }
        let exported = self
            .identities()
            .components()
            .iter()
            .flat_map(|component| component.units.iter().map(SourceUnitKey::from))
            .collect::<BTreeSet<_>>();
        let mut ineligible_units = Vec::new();
        for unit in self.identities().units() {
            let raw = index
                .raw
                .get(&(unit.file.as_str(), unit.source_uuid.as_str()))
                .ok_or("missing source disposition unit")?;
            let native_exported = exported.contains(&SourceUnitKey::from(unit));
            let on_board = raw.on_board.unwrap_or(true);
            if !native_exported || !on_board || unit.reference.starts_with('#') {
                let record = SynchronizationIneligibleUnit {
                    file: unit.file.clone(),
                    sheet_path: unit.sheet_path.clone(),
                    source_uuid: unit.source_uuid.clone(),
                    reference: unit.reference.clone(),
                    unit: unit.unit,
                    alias: format!("{}/{}", unit.sheet_path, unit.source_uuid),
                    native_exported,
                    on_board,
                };
                metadata_bytes = metadata_bytes
                    .checked_add(
                        serde_json::to_vec(&record)
                            .map_err(|e| e.to_string())?
                            .len(),
                    )
                    .ok_or("source disposition metadata size overflow")?;
                if metadata_bytes > 128 * 1024 * 1024 {
                    return Err("expanded source disposition metadata exceeds bound".into());
                }
                ineligible_units.push(record);
            }
        }
        let bytes = serde_json::to_vec(&serde_json::json!({"source":self.revision(),
            "components":components,"ineligible_units":ineligible_units,"connectivity":connectivity.evidence()}))
        .map_err(|e| e.to_string())?;
        self.verify()?;
        Ok(SynchronizationSource {
            audit: self.clone(),
            connectivity,
            components: components.into(),
            ineligible_units: ineligible_units.into(),
            valid: Arc::new(AtomicBool::new(true)),
            digest: hash(&bytes),
        })
    }
}

struct SourceIndex<'a> {
    native: BTreeMap<&'a str, &'a kicad_schematic::NetlistComponent>,
    pins: BTreeMap<Arc<[SourceUnitKey]>, Vec<&'a kicad_schematic::pins::LogicalPin>>,
    raw: BTreeMap<(&'a str, &'a str), &'a kicad_schematic::RawSymbol>,
    fields: BTreeMap<&'a str, BTreeMap<String, BTreeMap<String, String>>>,
}
impl<'a> SourceIndex<'a> {
    fn new(audit: &'a SchematicAudit) -> Result<Self, String> {
        let native = audit
            .netlist()
            .components
            .iter()
            .map(|c| (c.reference.as_str(), c))
            .collect();
        let mut pins = BTreeMap::<_, Vec<_>>::new();
        for pin in audit.pins().pins() {
            pins.entry(pin.key.component_units.clone())
                .or_default()
                .push(pin);
        }
        for component in audit.identities().components() {
            let mut units = component
                .units
                .iter()
                .map(SourceUnitKey::from)
                .collect::<Vec<_>>();
            units.sort();
            let expected = audit
                .pins()
                .component_pin_numbers(&units)
                .ok_or("missing complete source component pin universe")?;
            let observed = pins.entry(Arc::<[SourceUnitKey]>::from(units)).or_default();
            if observed
                .iter()
                .map(|pin| pin.key.number.clone())
                .collect::<BTreeSet<_>>()
                != *expected
            {
                return Err("logical pin records differ from complete component universe".into());
            }
        }
        let mut raw = BTreeMap::new();
        let mut fields = BTreeMap::new();
        for file in &audit.inner.parsed.files {
            let name = file.artifact.relative_path.as_str();
            let original = audit
                .inner
                .source
                .files
                .get(&audit.inner.policy.project.join(name))
                .and_then(|f| f.bytes.as_deref())
                .ok_or("uncaptured source component bytes")?;
            let parsed = component_fields(original)?;
            if parsed.len() != file.symbols.len() {
                return Err("incomplete captured source field index".into());
            }
            for symbol in &file.symbols {
                if raw
                    .insert((name, symbol.source_uuid.as_str()), symbol)
                    .is_some()
                {
                    return Err("duplicate physical source symbol".into());
                }
            }
            fields.insert(name, parsed);
        }
        Ok(Self {
            native,
            pins,
            raw,
            fields,
        })
    }
}

fn source_component(
    identity: &ComponentIdentity,
    index: &SourceIndex<'_>,
) -> Result<SynchronizationComponent, String> {
    let native = *index
        .native
        .get(identity.reference.as_str())
        .ok_or("missing native synchronization component")?;
    let keys = identity
        .units
        .iter()
        .map(SourceUnitKey::from)
        .collect::<BTreeSet<_>>();
    let unit_ids = native
        .symbol_uuid
        .as_deref()
        .ok_or("missing ordered native unit identities")?
        .split_whitespace()
        .collect::<Vec<_>>();
    let aliases = identity
        .units
        .iter()
        .map(|u| format!("{}/{}", u.sheet_path, u.source_uuid))
        .collect::<BTreeSet<_>>();
    if keys.is_empty() || unit_ids.len() != keys.len() {
        return Err("incomplete synchronization component membership".into());
    }
    let canonical_path = format!(
        "{}/{}",
        identity.sheet_path,
        unit_ids.first().ok_or("empty native units")?
    );
    if !aliases.contains(&canonical_path) {
        return Err("native canonical component alias is not authoritative".into());
    }
    let mut pins = BTreeMap::new();
    let ordered_keys = keys.iter().cloned().collect::<Vec<_>>();
    for pin in index
        .pins
        .get(ordered_keys.as_slice())
        .ok_or("missing complete source component pin index")?
    {
        {
            if pin
                .key
                .component_units
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                != keys
                || pin.reference != identity.reference
            {
                return Err("partial synchronization pin ownership".into());
            }
            let net = SynchronizationNet {
                name: pin.net.name.clone(),
                class: pin.net.class.clone(),
                electrical_no_connect: pin.net.electrical_type == "no_connect",
                explicit_no_connect: pin.net.explicit_no_connect,
                pin_function: pin.net.pin_function.clone(),
                pin_type: if pin.net.explicit_no_connect {
                    format!("{}+no_connect", pin.net.electrical_type)
                } else {
                    pin.net.electrical_type.clone()
                },
            };
            if pins.insert(pin.key.number.clone(), net).is_some() {
                return Err("duplicate logical synchronization pad number".into());
            }
        }
    }
    let units = native
        .units
        .as_ref()
        .ok_or("missing complete native unit metadata")?
        .iter()
        .map(|u| SynchronizationUnit {
            name: u.name.clone(),
            pins: u.pins.clone(),
        })
        .collect::<Vec<_>>();
    if units.is_empty() {
        return Err("empty native component unit metadata".into());
    }
    if pins.is_empty() && units.iter().any(|unit| !unit.pins.is_empty()) {
        return Err(
            "empty active pin universe requires complete zero-pin native unit metadata".into(),
        );
    }
    let mut shared = None;
    let mut common_fields = None::<BTreeMap<String, String>>;
    let mut all_fields = BTreeSet::new();
    let mut conflicting_fields = BTreeSet::new();
    for unit in &identity.units {
        let raw = index
            .raw
            .get(&(unit.file.as_str(), unit.source_uuid.as_str()))
            .ok_or("missing source unit")?;
        let flags = (
            raw.in_bom.unwrap_or(true),
            raw.on_board.unwrap_or(true),
            raw.dnp.unwrap_or(false),
        );
        if shared
            .replace(flags)
            .is_some_and(|previous| previous != flags)
        {
            return Err("source component flags disagree across units".into());
        }
        let fields = index
            .fields
            .get(unit.file.as_str())
            .and_then(|file| file.get(&unit.source_uuid))
            .ok_or("missing captured source component fields")?;
        all_fields.extend(fields.keys().cloned());
        if let Some(common) = &mut common_fields {
            for (name, value) in common.iter() {
                if fields.get(name) != Some(value) {
                    conflicting_fields.insert(name.clone());
                }
            }
            for (name, value) in fields {
                if common.get(name) != Some(value) {
                    conflicting_fields.insert(name.clone());
                }
            }
            common.retain(|name, value| fields.get(name) == Some(value));
        } else {
            common_fields = Some(fields.clone());
        }
    }
    let (in_bom, on_board, dnp) = shared.ok_or("missing component flags")?;
    let mut fields = common_fields.ok_or("missing component fields")?;
    for name in ["Reference", "Value", "Footprint"] {
        fields.remove(name);
        conflicting_fields.remove(name);
        all_fields.remove(name);
    }
    conflicting_fields.extend(
        all_fields
            .into_iter()
            .filter(|name| !fields.contains_key(name)),
    );
    let identity_bytes = serde_json::to_vec(
        &identity
            .units
            .iter()
            .map(|u| (&u.file, &u.sheet_path, &u.source_uuid))
            .collect::<Vec<_>>(),
    )
    .map_err(|e| e.to_string())?;
    Ok(SynchronizationComponent {
        id: hash(&identity_bytes),
        reference: native.reference.clone(),
        value: native.value.clone(),
        footprint: native.footprint.clone(),
        aliases: aliases.into_iter().collect(),
        canonical_path,
        sheet_name: native.sheet_path.clone(),
        sheet_file: identity
            .units
            .first()
            .ok_or("missing component source")?
            .file
            .clone(),
        units,
        placed_units: identity.units.iter().map(|u| u.unit).collect(),
        pins,
        in_bom,
        on_board,
        dnp,
        fields,
        conflicting_fields,
    })
}

fn component_fields(bytes: &[u8]) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let doc = kicad_schematic::cst::parse_one(text).map_err(|e| e.to_string())?;
    let list = source_items;
    let atom = source_atom;
    let root =
        list(doc.nodes.first().ok_or("missing source root")?).ok_or("invalid source root")?;
    let mut result = BTreeMap::new();
    for node in root
        .iter()
        .filter_map(list)
        .filter(|n| n.first().and_then(atom) == Some("symbol"))
    {
        let ids = node
            .iter()
            .filter_map(list)
            .filter(|n| n.first().and_then(atom) == Some("uuid"))
            .collect::<Vec<_>>();
        if ids.len() != 1 {
            return Err("missing or duplicate captured symbol identity".into());
        }
        let uuid = ids[0]
            .get(1)
            .and_then(atom)
            .ok_or("missing captured source symbol UUID")?;
        let mut fields = BTreeMap::new();
        for property in node
            .iter()
            .filter_map(list)
            .filter(|n| n.first().and_then(atom) == Some("property"))
        {
            let name = property
                .get(1)
                .and_then(atom)
                .ok_or("missing source field name")?;
            let value = property
                .get(2)
                .and_then(atom)
                .ok_or("missing source field value")?;
            if fields.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err("duplicate source component field".into());
            }
        }
        if result.insert(uuid.into(), fields).is_some() {
            return Err("duplicate source component UUID".into());
        }
    }
    Ok(result)
}

fn source_items(node: &kiutils_sexpr::Node) -> Option<&[kiutils_sexpr::Node]> {
    board::items(node).ok()
}
fn source_atom(node: &kiutils_sexpr::Node) -> Option<&str> {
    board::atom(node).ok()
}
