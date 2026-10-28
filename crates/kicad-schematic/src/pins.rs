//! Source-backed logical component pins and complete native net membership.
use crate::cst::parse_one;
use crate::{
    NetlistDocument, ParsedSchematicProject, RawSymbol, atom_text,
    geometry::{Point, SymbolTransform},
    identities::{SchematicIdentityIndex, SymbolUnitIdentity, uuid},
    list_items, list_tag, parse_schematic,
};
use kiutils_sexpr::Node;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const MAX_PINS: usize = 1_000_000;
const MAX_STACK: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SourceUnitKey {
    pub file: String,
    pub sheet_path: String,
    pub symbol_uuid: String,
}
impl From<&SymbolUnitIdentity> for SourceUnitKey {
    fn from(unit: &SymbolUnitIdentity) -> Self {
        Self {
            file: unit.file.clone(),
            sheet_path: unit.sheet_path.clone(),
            symbol_uuid: unit.source_uuid.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct LogicalPinKey {
    pub component_units: Arc<[SourceUnitKey]>,
    pub number: String,
}

/// Stored records are provenance for a graphical number, not a claim that every record is active.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PinOccurrence {
    pub source: SourceUnitKey,
    pub unit: u32,
    pub graphical_number: String,
    pub stored_pin_uuids: Arc<[String]>,
    pub name: String,
    pub electrical_type: String,
    pub library_position: Point,
    pub position: Point,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePinNet {
    pub code: String,
    pub name: String,
    pub class: Option<String>,
    pub pin_function: String,
    pub electrical_type: String,
    pub explicit_no_connect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalPin {
    pub key: LogicalPinKey,
    pub reference: String,
    pub occurrences: Vec<PinOccurrence>,
    pub net: NativePinNet,
}

/// These source pins have no component in the native XML; their connectivity is unproven.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnexportedPin {
    pub reference: String,
    pub number: String,
    pub occurrence: PinOccurrence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchematicPinIndex {
    pins: Vec<LogicalPin>,
    unexported: Vec<UnexportedPin>,
}

#[derive(Clone, Debug)]
struct SourcePin {
    number: String,
    graphical_number: String,
    records: Arc<[String]>,
    name: String,
    kind: String,
    native_function: String,
    library_position: Point,
    position: Point,
}
struct SourceUnit {
    all_styles: BTreeSet<String>,
    active: Vec<SourcePin>,
}

fn children<'a>(items: &'a [Node], tag: &'a str) -> impl Iterator<Item = &'a [Node]> {
    items
        .iter()
        .filter_map(list_items)
        .filter(move |n| list_tag(n) == Some(tag))
}
fn one<'a>(items: &'a [Node], tag: &'a str) -> Result<&'a [Node], String> {
    let mut values = children(items, tag);
    let result = values
        .next()
        .ok_or_else(|| format!("missing pin evidence field: {tag}"))?;
    if values.next().is_some() {
        return Err(format!("duplicate pin evidence field: {tag}"));
    }
    Ok(result)
}
fn text(items: &[Node], index: usize) -> Result<&str, String> {
    atom_text(items.get(index)).ok_or_else(|| "missing pin evidence atom".into())
}
fn field<'a>(items: &'a [Node], tag: &'a str) -> Result<&'a str, String> {
    text(one(items, tag)?, 1)
}

fn numbers(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() || value.contains('$') || value.chars().any(char::is_control) {
        return Err("empty, dynamic or invalid pin number".into());
    }
    if !value.contains(['[', ']']) {
        return Ok(vec![value.into()]);
    }
    let inner = value
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or("malformed stacked pin number")?;
    if inner.contains(['[', ']']) {
        return Err("nested stacked pin number".into());
    }
    let mut result = Vec::new();
    for item in inner.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((first, last)) = item.split_once('-') {
            let suffix = |s: &str| -> Result<(String, u32), String> {
                let s = s.trim();
                let start = s.trim_end_matches(|c: char| c.is_ascii_digit()).len();
                Ok((
                    s[..start].into(),
                    s[start..].parse().map_err(|_| "invalid stacked range")?,
                ))
            };
            let (prefix, a) = suffix(first)?;
            let (other, b) = suffix(last)?;
            if prefix != other || b < a || (b - a) as usize >= MAX_STACK {
                return Err("invalid or excessive stacked range".into());
            }
            for n in a..=b {
                result.push(format!("{prefix}{n}"));
            }
        } else {
            result.push(item.into());
        }
        if result.len() > MAX_STACK {
            return Err("stacked pin count exceeds bound".into());
        }
    }
    if result.is_empty() || result.iter().collect::<BTreeSet<_>>().len() != result.len() {
        return Err("empty or repeated stacked pin number".into());
    }
    Ok(result)
}

fn definition<'a>(
    definitions: &'a BTreeMap<String, &'a [Node]>,
    name: &str,
) -> Result<&'a [Node], String> {
    let mut name = name;
    let mut seen = BTreeSet::new();
    loop {
        if seen.len() >= 32 || !seen.insert(name.to_owned()) {
            return Err("embedded symbol inheritance cycle/depth".into());
        }
        let current = *definitions
            .get(name)
            .ok_or("missing embedded symbol definition")?;
        if children(current, "extends").next().is_none() {
            return Ok(current);
        }
        if children(current, "symbol").any(|body| children(body, "pin").next().is_some()) {
            return Err("derived embedded symbol has ambiguous pin overrides".into());
        }
        name = field(current, "extends")?;
    }
}

fn source_unit(
    raw: &RawSymbol,
    source: &[Node],
    def: &[Node],
    format: u32,
) -> Result<SourceUnit, String> {
    let transform = SymbolTransform::from_cst(source)?;
    let mut record_lists = BTreeMap::<&str, Vec<String>>::new();
    for pin in &raw.pins {
        record_lists
            .entry(&pin.number)
            .or_default()
            .push(pin.source_uuid.clone());
    }
    let stored_records = record_lists
        .into_iter()
        .map(|(number, mut records)| {
            records.sort();
            (number, Arc::<[String]>::from(records))
        })
        .collect::<BTreeMap<_, _>>();
    let mut active = Vec::new();
    let mut all_styles = BTreeSet::new();
    let style = raw.body_style.unwrap_or(1);
    if style == 0 {
        return Err("zero active body style".into());
    }
    let mut alternates = BTreeMap::<String, String>::new();
    for pin in children(source, "pin") {
        let number = text(pin, 1)?;
        if children(pin, "alternate").next().is_some() {
            let value = field(pin, "alternate")?;
            if let Some(previous) = alternates.insert(number.into(), value.into())
                && previous != value
            {
                return Err("ambiguous stored alternate pin functions".into());
            }
        }
    }
    for body in children(def, "symbol") {
        let mut parts = text(body, 1)?.rsplitn(3, '_');
        let body_style: u32 = parts
            .next()
            .ok_or("missing embedded body style")?
            .parse()
            .map_err(|_| "invalid embedded body style")?;
        let unit: u32 = parts
            .next()
            .ok_or("missing embedded unit")?
            .parse()
            .map_err(|_| "invalid embedded unit")?;
        if parts.next().is_none() {
            return Err("invalid embedded unit name".into());
        }
        if unit != 0 && unit != raw.unit {
            continue;
        }
        for pin in children(body, "pin") {
            let graphical = field(pin, "number")?;
            let expanded = numbers(graphical)?;
            all_styles.extend(expanded.iter().cloned());
            if all_styles.len() > MAX_PINS {
                return Err("unit pin inventory exceeds bound".into());
            }
            if body_style != 0 && body_style != style {
                continue;
            }
            let at = one(pin, "at")?;
            if at.len() != 4 {
                return Err("invalid library pin contact position".into());
            }
            let library_position = Point::from_at(at)?;
            let position = transform.contact(library_position)?;
            let name = field(pin, "name")?;
            let mut name = if format < 20250318 && name == "~" {
                ""
            } else {
                name
            };
            let mut kind = text(pin, 1)?;
            if let Some(alt) = alternates.get(graphical) {
                let matching = children(pin, "alternate")
                    .filter(|a| atom_text(a.get(1)) == Some(alt.as_str()))
                    .collect::<Vec<_>>();
                if matching.len() != 1 {
                    return Err("missing or ambiguous alternate pin definition".into());
                }
                name = text(matching[0], 1)?;
                kind = text(matching[0], 2)?;
            }
            if name.contains('$') {
                return Err("dynamic pin function requires expansion evidence".into());
            }
            let records = stored_records.get(graphical).cloned().unwrap_or_default();
            if records.is_empty() {
                return Err("active graphical pin has no stored UUID record".into());
            }
            for number in &expanded {
                if active.len() >= MAX_PINS {
                    return Err("active pin count exceeds bound".into());
                }
                let native_function = if !name.is_empty() {
                    format!("{name}_{number}")
                } else if expanded.len() > 1 {
                    number.clone()
                } else {
                    String::new()
                };
                active.push(SourcePin {
                    number: number.clone(),
                    graphical_number: graphical.into(),
                    records: records.clone(),
                    name: name.into(),
                    kind: kind.into(),
                    native_function,
                    library_position,
                    position,
                });
            }
        }
    }
    Ok(SourceUnit { all_styles, active })
}

fn source_inventory(
    project: &ParsedSchematicProject,
    sources: &BTreeMap<String, &[u8]>,
) -> Result<BTreeMap<(String, String), SourceUnit>, String> {
    if sources.len() != project.files.len() {
        return Err("source pin file set differs".into());
    }
    let mut result = BTreeMap::new();
    let mut total = 0_u64;
    let mut pin_count = 0_usize;
    for file in &project.files {
        let bytes = sources
            .get(&file.artifact.relative_path)
            .ok_or("missing exact pin source bytes")?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or("source byte overflow")?;
        if bytes.len() > 32 * 1024 * 1024 || total > 128 * 1024 * 1024 {
            return Err("pin source bytes exceed bound".into());
        }
        if parse_schematic(&file.artifact.relative_path, bytes).map_err(|e| e.to_string())? != *file
        {
            return Err("pin source bytes differ from observed revision".into());
        }
        let document = parse_one(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let root = list_items(&document.nodes[0]).ok_or("invalid pin source root")?;
        let library = one(root, "lib_symbols")?;
        let mut definitions = BTreeMap::new();
        for def in children(library, "symbol") {
            if definitions.len() >= 100_000
                || definitions.insert(text(def, 1)?.to_owned(), def).is_some()
            {
                return Err("duplicate or excessive embedded symbols".into());
            }
        }
        let placed = children(root, "symbol")
            .map(|s| Ok((field(s, "uuid")?, s)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut ids = file
            .symbols
            .iter()
            .map(|s| s.source_uuid.as_str())
            .chain(file.sheets.iter().map(|s| s.source_uuid.as_str()))
            .chain(std::iter::once(file.file_uuid.as_str()))
            .collect::<BTreeSet<_>>();
        for raw in &file.symbols {
            for pin in &raw.pins {
                if !uuid(&pin.source_uuid) || !ids.insert(pin.source_uuid.as_str()) {
                    return Err("invalid or reused stored pin UUID".into());
                }
            }
            let def = definition(&definitions, raw.lib_name.as_deref().unwrap_or(&raw.lib_id))?;
            let source = placed
                .get(raw.source_uuid.as_str())
                .ok_or("missing placed symbol source")?;
            let format = file
                .format_version
                .parse::<u32>()
                .map_err(|_| "invalid pin source format version")?;
            let unit = source_unit(raw, source, def, format)?;
            pin_count += unit.active.len() + unit.all_styles.len();
            if pin_count > MAX_PINS {
                return Err("physical pin inventory exceeds bound".into());
            }
            result.insert(
                (file.artifact.relative_path.clone(), raw.source_uuid.clone()),
                unit,
            );
        }
    }
    Ok(result)
}

fn occurrence(unit: &SymbolUnitIdentity, pin: &SourcePin) -> PinOccurrence {
    PinOccurrence {
        source: unit.into(),
        unit: unit.unit,
        graphical_number: pin.graphical_number.clone(),
        stored_pin_uuids: pin.records.clone(),
        name: pin.name.clone(),
        electrical_type: pin.kind.clone(),
        library_position: pin.library_position,
        position: pin.position,
    }
}

impl SchematicPinIndex {
    pub fn pins(&self) -> &[LogicalPin] {
        &self.pins
    }
    pub fn unexported(&self) -> &[UnexportedPin] {
        &self.unexported
    }

    /// Net partitions use source identities, so reference renaming does not alter membership.
    pub fn connectivity(&self) -> Vec<Vec<LogicalPinKey>> {
        let mut groups = BTreeMap::<&str, Vec<LogicalPinKey>>::new();
        for pin in &self.pins {
            groups
                .entry(&pin.net.code)
                .or_default()
                .push(pin.key.clone());
        }
        let mut result = groups.into_values().collect::<Vec<_>>();
        for group in &mut result {
            group.sort();
        }
        result.sort();
        result
    }

    pub fn resolve(
        project: &ParsedSchematicProject,
        netlist: &NetlistDocument,
        sources: &BTreeMap<String, &[u8]>,
    ) -> Result<Self, String> {
        let identities =
            SchematicIdentityIndex::resolve(project, netlist).map_err(|e| e.to_string())?;
        let source = source_inventory(project, sources)?;
        let components = netlist
            .components
            .iter()
            .map(|c| (c.reference.as_str(), c))
            .collect::<BTreeMap<_, _>>();
        let mut nodes = BTreeMap::new();
        let mut codes = BTreeSet::new();
        for net in &netlist.nets {
            if net.code.is_empty() || !codes.insert(&net.code) {
                return Err("empty or duplicate native net code".into());
            }
            for node in &net.nodes {
                if nodes.len() >= MAX_PINS
                    || nodes
                        .insert((node.reference.clone(), node.pin.clone()), (net, node))
                        .is_some()
                {
                    return Err(
                        "native pin has duplicate/multiple net membership or exceeds bound".into(),
                    );
                }
            }
        }
        let mut pins = Vec::new();
        let mut expanded_count = 0_usize;
        let mut exported_units = BTreeSet::new();
        for component in identities.components() {
            let native = components[component.reference.as_str()];
            let metadata = native
                .units
                .as_ref()
                .ok_or("native unit pin metadata unavailable")?;
            let mut unit_keys = component
                .units
                .iter()
                .map(SourceUnitKey::from)
                .collect::<Vec<_>>();
            unit_keys.sort();
            let unit_keys: Arc<[SourceUnitKey]> = unit_keys.into();
            let mut active = BTreeMap::<String, Vec<(&SymbolUnitIdentity, &SourcePin)>>::new();
            for unit in &component.units {
                exported_units.insert(SourceUnitKey::from(unit));
                let info = &source[&(unit.file.clone(), unit.source_uuid.clone())];
                let slot = metadata
                    .get(unit.unit as usize - 1)
                    .ok_or("missing native unit slot")?;
                let set = slot.pins.iter().cloned().collect::<BTreeSet<_>>();
                if set.len() != slot.pins.len() || set != info.all_styles {
                    return Err("native unit pin set differs from embedded definition".into());
                }
                for pin in &info.active {
                    expanded_count += 1;
                    if expanded_count > MAX_PINS {
                        return Err("expanded pin occurrences exceed bound".into());
                    }
                    active
                        .entry(pin.number.clone())
                        .or_default()
                        .push((unit, pin));
                }
            }
            for (number, occurrences) in active {
                let (net, node) = nodes
                    .remove(&(component.reference.clone(), number.clone()))
                    .ok_or("active source pin missing from native netlist")?;
                let (kind, no_connect) = node
                    .pin_type
                    .strip_suffix("+no_connect")
                    .map_or((node.pin_type.as_str(), false), |s| (s, true));
                if occurrences
                    .iter()
                    .any(|(_, p)| p.kind != kind || p.native_function != node.pin_function)
                {
                    return Err("native pin type/function differs from active source pin".into());
                }
                let mut records = occurrences
                    .iter()
                    .map(|(u, p)| occurrence(u, p))
                    .collect::<Vec<_>>();
                records.sort();
                records.dedup();
                pins.push(LogicalPin {
                    key: LogicalPinKey {
                        component_units: unit_keys.clone(),
                        number,
                    },
                    reference: component.reference.clone(),
                    occurrences: records,
                    net: NativePinNet {
                        code: net.code.clone(),
                        name: net.name.clone(),
                        class: net.class.clone(),
                        pin_function: node.pin_function.clone(),
                        electrical_type: kind.into(),
                        explicit_no_connect: no_connect,
                    },
                });
            }
        }
        if !nodes.is_empty() {
            return Err("native netlist contains pins without active source evidence".into());
        }
        let mut unexported = Vec::new();
        for unit in identities.units() {
            if exported_units.contains(&SourceUnitKey::from(unit)) {
                continue;
            }
            for pin in &source[&(unit.file.clone(), unit.source_uuid.clone())].active {
                if unexported.len() + pins.len() >= MAX_PINS {
                    return Err("expanded source pins exceed bound".into());
                }
                unexported.push(UnexportedPin {
                    reference: unit.reference.clone(),
                    number: pin.number.clone(),
                    occurrence: occurrence(unit, pin),
                });
            }
        }
        pins.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(Self { pins, unexported })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NetlistLimits, parse_netlist_xml};
    use kicad_model::SchematicArtifactRevision;

    fn fixture(reference: &str) -> (ParsedSchematicProject, NetlistDocument, Vec<u8>) {
        let root = "11111111-1111-4111-8111-111111111111";
        let source_id = |unit| format!("{unit:08x}-2222-4222-8222-222222222222");
        let mut symbols = String::new();
        for unit in 1..=2 {
            let pins = [1, 2, 7, 9]
                .into_iter()
                .map(|n| format!("(pin \"{n}\" (uuid \"{unit:08x}-3333-4333-8333-{n:012x}\"))"))
                .collect::<String>();
            symbols.push_str(&format!(r#"(symbol (uuid "{}") (lib_id "Test:Multi") (unit {unit}) (at 10 10 0) (property "Reference" "{reference}") (property "Value" "Multi") {pins} (instances (project "pins" (path "/{root}" (reference "{reference}") (unit {unit})))))"#,source_id(unit)));
        }
        let bytes = format!(r#"(kicad_sch (version 20260306) (generator eeschema) (uuid "{root}")
            (lib_symbols (symbol "Test:Multi"
                (symbol "Multi_0_0" (pin power_in line (at 1 2 0) (number "7") (name "VCC")))
                (symbol "Multi_1_1" (pin passive line (at 1 2 0) (number "1") (name "A")))
                (symbol "Multi_1_2" (pin input line (at 1 2 0) (number "1") (name "B")))
                (symbol "Multi_2_1" (pin output line (at 1 2 0) (number "2") (name "C")))
                (symbol "Multi_3_1" (pin passive line (at 1 2 0) (number "9") (name "Unplaced"))))) {symbols})"#).into_bytes();
        let parsed = parse_schematic("pins.kicad_sch", &bytes).unwrap();
        let project = ParsedSchematicProject {
            root_relative_path: "pins.kicad_sch".into(),
            revision: SchematicArtifactRevision::new(
                vec![parsed.artifact.clone()],
                "10.0.7",
                1,
                false,
            )
            .unwrap(),
            files: vec![parsed],
            editor_lock_present: false,
        };
        let xml = format!(
            r#"<export version="E"><design><sheet number="1" name="/" tstamps="/"/></design><components><comp ref="{reference}"><value>Multi</value><libsource lib="Test" part="Multi"/><sheetpath names="/" tstamps="/"/><tstamps>{} {}</tstamps><units><unit name="Input"><pins><pin num="1"/><pin num="7"/></pins></unit><unit name="Output"><pins><pin num="2"/><pin num="7"/></pins></unit><unit name="Unplaced"><pins><pin num="9"/><pin num="7"/></pins></unit></units></comp></components><nets><net code="1" name="Open"><node ref="{reference}" pin="1" pinfunction="A_1" pintype="passive+no_connect"/></net><net code="2" name="Output"><node ref="{reference}" pin="2" pinfunction="C_2" pintype="output"/></net><net code="3" name="Supply"><node ref="{reference}" pin="7" pinfunction="VCC_7" pintype="power_in"/></net></nets></export>"#,
            source_id(1),
            source_id(2)
        );
        (
            project,
            parse_netlist_xml(xml.as_bytes(), NetlistLimits::default()).unwrap(),
            bytes,
        )
    }

    fn resolve(
        project: &ParsedSchematicProject,
        netlist: &NetlistDocument,
        bytes: &[u8],
    ) -> Result<SchematicPinIndex, String> {
        SchematicPinIndex::resolve(
            project,
            netlist,
            &BTreeMap::from([("pins.kicad_sch".into(), bytes)]),
        )
    }

    #[test]
    fn active_units_share_common_pins_and_preserve_reference_independent_membership() {
        let (project, netlist, bytes) = fixture("U1");
        let index = resolve(&project, &netlist, &bytes).unwrap();
        assert_eq!(index.pins().len(), 3);
        assert_eq!(
            index
                .pins()
                .iter()
                .find(|p| p.key.number == "7")
                .unwrap()
                .occurrences
                .len(),
            2
        );
        assert!(
            index
                .pins()
                .iter()
                .find(|p| p.key.number == "1")
                .unwrap()
                .net
                .explicit_no_connect
        );
        assert!(
            !index
                .pins()
                .iter()
                .find(|p| p.key.number == "2")
                .unwrap()
                .net
                .explicit_no_connect
        );
        assert!(index.pins().iter().all(|p| p.key.number != "9"));
        let (renamed, native, bytes) = fixture("U300");
        assert_eq!(
            index.connectivity(),
            resolve(&renamed, &native, &bytes).unwrap().connectivity()
        );
    }

    #[test]
    fn pin_bijection_rejects_missing_extra_duplicate_and_conflicting_native_evidence() {
        let (project, netlist, bytes) = fixture("U1");
        let mut missing = netlist.clone();
        missing.nets[0].nodes.clear();
        let mut extra = netlist.clone();
        extra.nets[0].nodes[0].pin = "9".into();
        let mut duplicate = netlist.clone();
        duplicate.nets[1]
            .nodes
            .push(netlist.nets[0].nodes[0].clone());
        let mut kind = netlist.clone();
        kind.nets[0].nodes[0].pin_type = "output".into();
        let mut function = netlist.clone();
        function.nets[0].nodes[0].pin_function = "B_1".into();
        let mut metadata = netlist.clone();
        metadata.components[0].units.as_mut().unwrap()[0]
            .pins
            .push("9".into());
        let mut absent = netlist.clone();
        absent.components[0].units = None;
        let mut repeated = netlist.clone();
        repeated.components[0].units.as_mut().unwrap()[0]
            .pins
            .push("1".into());
        for bad in [
            missing, extra, duplicate, kind, function, metadata, absent, repeated,
        ] {
            assert!(resolve(&project, &bad, &bytes).is_err());
        }
        let mut stale = bytes;
        stale.push(b'\n');
        assert!(resolve(&project, &netlist, &stale).is_err());
    }

    #[test]
    fn legacy_and_current_body_style_tokens_share_one_unambiguous_field() {
        let (_, _, bytes) = fixture("U1");
        let text = String::from_utf8(bytes).unwrap();
        let legacy = text.replace("(at 10 10 0)", "(at 10 10 0) (convert 2)");
        let parsed = parse_schematic("pins.kicad_sch", legacy.as_bytes()).unwrap();
        assert!(parsed.symbols.iter().all(|s| s.body_style == Some(2)));
        let repeated = legacy.replace("(convert 2)", "(convert 2) (body_style 1)");
        assert!(parse_schematic("pins.kicad_sch", repeated.as_bytes()).is_err());
    }

    #[test]
    fn pin_name_normalization_is_format_bound_and_does_not_rewrite_alternates() {
        for format in [20250114, 20260306] {
            for alternate in [false, true] {
                let (mut project, mut native, bytes) = fixture("U1");
                let mut text = String::from_utf8(bytes)
                    .unwrap()
                    .replace("20260306", &format.to_string());
                if alternate {
                    text = text.replace(
                        "(name \"A\")",
                        "(name \"A\") (alternate \"~\" passive line)",
                    );
                    text = text.replace("(pin \"1\" (uuid", "(pin \"1\" (alternate \"~\") (uuid");
                } else {
                    text = text.replace("(name \"A\")", "(name \"~\")");
                }
                let parsed = parse_schematic("pins.kicad_sch", text.as_bytes()).unwrap();
                project.revision = SchematicArtifactRevision::new(
                    vec![parsed.artifact.clone()],
                    "10.0.7",
                    1,
                    false,
                )
                .unwrap();
                project.files = vec![parsed];
                native.nets[0].nodes[0].pin_function = if alternate || format >= 20250318 {
                    "~_1".into()
                } else {
                    String::new()
                };
                assert!(resolve(&project, &native, text.as_bytes()).is_ok());
                native.nets[0].nodes[0].pin_function = "incorrect".into();
                assert!(resolve(&project, &native, text.as_bytes()).is_err());
            }
        }
    }

    #[test]
    fn stacked_ranges_are_bounded_and_unambiguous() {
        assert_eq!(numbers("[A1-A3,9]").unwrap(), ["A1", "A2", "A3", "9"]);
        assert_eq!(numbers("A-1").unwrap(), ["A-1"]);
        for bad in [
            "",
            "[]",
            "[A3-A1]",
            "[A1-B2]",
            "[1-999999999]",
            "[1,1]",
            "[A1",
            "[[1]]",
            "${PIN}",
        ] {
            assert!(numbers(bad).is_err(), "{bad}");
        }
    }
}
