use kicad_model::{
    AllowlistedFootprintPlacement, ObservedFootprintPlacement, ObservedSchematicSymbol,
    ProjectSnapshot, RevisionDigest, StableId,
};
use kicad_schematic::{NetlistDocument, ParsedSchematicProject};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchematicFootprintBinding {
    pub schematic_revision: RevisionDigest,
    pub board_revision: RevisionDigest,
    pub symbol: ObservedSchematicSymbol,
    pub symbol_path: Vec<StableId>,
    pub pad_nets: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PinPadBinding {
    pub pin_id: StableId,
    pub pad_id: StableId,
    pub footprint_placement_id: StableId,
    pub net_name: Option<String>,
}

pub fn bind_schematic_footprint(
    project: &ParsedSchematicProject,
    netlist: &NetlistDocument,
    board: &ProjectSnapshot,
    placement: &AllowlistedFootprintPlacement,
) -> Result<SchematicFootprintBinding, String> {
    placement.validate().map_err(|e| e.to_string())?;
    board.validate().map_err(|e| e.to_string())?;
    let observation = crate::schematic_observation(project, netlist, board.project_id.clone())
        .map_err(|e| e.to_string())?;
    if !observation.baseline_eligible {
        return Err(
            "schematic editor must be closed before binding a saved footprint source".into(),
        );
    }
    let matches = observation
        .symbols
        .iter()
        .filter(|symbol| symbol.reference == placement.reference)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err("schematic reference is missing or ambiguous".into());
    }
    let symbol = matches[0];
    let identifier = format!(
        "{}:{}",
        placement.library_identifier.nickname, placement.library_identifier.entry_name
    );
    if symbol.footprint_field.as_deref() != Some(identifier.as_str())
        || symbol.value != placement.value
    {
        return Err("requested footprint or value differs from the saved schematic".into());
    }
    let components = netlist
        .components
        .iter()
        .filter(|component| component.reference == symbol.reference)
        .collect::<Vec<_>>();
    if components.len() != 1
        || components[0].footprint.as_deref() != Some(identifier.as_str())
        || components[0].value != symbol.value
    {
        return Err("netlist footprint or value differs from the schematic".into());
    }
    let raw = project
        .files
        .iter()
        .flat_map(|file| &file.symbols)
        .filter(|raw| {
            raw.source_uuid == symbol.source_uuid.as_str() && raw.reference == symbol.reference
        })
        .collect::<Vec<_>>();
    if raw.len() != 1
        || raw[0].footprint.as_deref() != Some(identifier.as_str())
        || raw[0].value != symbol.value
    {
        return Err("saved schematic fields differ from the netlist".into());
    }
    let mut symbol_path = symbol
        .sheet_instance_path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| StableId::new(part).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if symbol_path.is_empty() {
        return Err("symbol sheet path is empty".into());
    }
    symbol_path.push(symbol.source_uuid.clone());
    let observed = board
        .native_footprints
        .as_ref()
        .ok_or("native footprint observation is required")?;
    if observed.placements.iter().any(|existing| {
        existing.reference == symbol.reference || existing.symbol_sheet_path == symbol_path
    }) {
        return Err("schematic symbol already has a PCB footprint".into());
    }
    let mut pad_nets = BTreeMap::new();
    for pin in &symbol.pins {
        if pin.number.is_empty() || pad_nets.insert(pin.number.clone(), None).is_some() {
            return Err("schematic pin numbers are empty or ambiguous".into());
        }
    }
    if pad_nets.is_empty() {
        return Err("schematic symbol has no observed pins".into());
    }
    for net in &netlist.nets {
        for node in net
            .nodes
            .iter()
            .filter(|node| node.reference == symbol.reference)
        {
            let value = pad_nets
                .get_mut(&node.pin)
                .ok_or("netlist node has no observed schematic pin")?;
            if value.replace(net.name.clone()).is_some() {
                return Err("pin belongs to multiple nets".into());
            }
        }
    }
    Ok(SchematicFootprintBinding {
        schematic_revision: observation.revision,
        board_revision: board.revision().map_err(|e| e.to_string())?,
        symbol: symbol.clone(),
        symbol_path,
        pad_nets,
    })
}

pub fn verify_placed_pin_pad_binding(
    binding: &SchematicFootprintBinding,
    placement: &AllowlistedFootprintPlacement,
    actual: &ObservedFootprintPlacement,
    observed_board: &StableId,
) -> Result<Vec<PinPadBinding>, String> {
    actual.validate(observed_board).map_err(|e| e.to_string())?;
    if actual.reference != binding.symbol.reference
        || actual.value != binding.symbol.value
        || actual.library_identifier != placement.library_identifier
        || actual.position != placement.position
        || actual.symbol_sheet_path != binding.symbol_path
    {
        return Err("placed footprint identity or schematic association differs".into());
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for pad in &actual.pads {
        if pad.number.is_empty() {
            if pad.net.is_some() {
                return Err("mechanical pad unexpectedly carries a net".into());
            }
            continue;
        }
        let expected = binding
            .pad_nets
            .get(&pad.number)
            .ok_or("footprint pad has no schematic pin")?;
        let actual_net = pad.net.as_ref().map(|net| net.name.as_str());
        if actual_net != expected.as_deref() {
            return Err("placed pad network differs from the schematic".into());
        }
        let pins = binding
            .symbol
            .pins
            .iter()
            .filter(|pin| pin.number == pad.number)
            .collect::<Vec<_>>();
        if pins.len() != 1 {
            return Err("pad number has an ambiguous schematic pin".into());
        }
        seen.insert(pad.number.clone());
        result.push(PinPadBinding {
            pin_id: pins[0].pin_id.clone(),
            pad_id: pad.pad_id.clone(),
            footprint_placement_id: actual.placement_id.clone(),
            net_name: expected.clone(),
        });
    }
    if seen != binding.pad_nets.keys().cloned().collect() {
        return Err("schematic pins are missing footprint pads".into());
    }
    result.sort_by(|a, b| (&a.pin_id, &a.pad_id).cmp(&(&b.pin_id, &b.pad_id)));
    Ok(result)
}
