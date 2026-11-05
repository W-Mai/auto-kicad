//! Native source-class changes preserve every other logical source field.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceNetclass {
    pub class: Option<String>,
}

pub(super) fn source_netclasses(
    netlist: &NetlistDocument,
) -> Result<BTreeMap<String, SourceNetclass>, String> {
    let records = netlist
        .nets
        .iter()
        .map(|net| {
            (
                net.name.clone(),
                SourceNetclass {
                    class: net.class.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if records.len() != netlist.nets.len() {
        return Err("ambiguous native source net names".into());
    }
    Ok(records)
}

/// Conservative references include every original effective constituent. Ambiguous names reject.
pub(super) fn source_class_references(
    netlist: &NetlistDocument,
    classes: &BTreeMap<String, ConfiguredClass>,
) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    for name in netlist
        .nets
        .iter()
        .filter_map(|net| net.class.as_deref())
        .filter(|s| !s.is_empty())
    {
        let parts = name.split(',').map(str::to_owned).collect::<Vec<_>>();
        let composite = parts.len() > 1 && parts.iter().all(|part| classes.contains_key(part));
        if classes.contains_key(name) {
            if composite {
                return Err("ambiguous native source composite class name".into());
            }
            result.insert(name.to_owned());
        } else if composite {
            result.extend(parts);
        } else {
            return Err("native source class lacks complete configured identity".into());
        }
    }
    Ok(result)
}

fn verify_netlist_classes(before: &NetlistDocument, after: &NetlistDocument) -> Result<(), String> {
    let after_classes = source_netclasses(after)?;
    source_netclasses(before)?;
    let mut projected = before.clone();
    for net in &mut projected.nets {
        net.class = after_classes
            .get(&net.name)
            .ok_or("project changed source net membership")?
            .class
            .clone();
    }
    if projected != *after {
        return Err("project changed native source data outside selected netclass records".into());
    }
    Ok(())
}

fn verify_source_class_records(
    before: &SynchronizationSource,
    after: &SynchronizationSource,
) -> Result<BTreeMap<String, NetclassRecordChange<SourceNetclass>>, String> {
    let old = &before.audit;
    let new = &after.audit;
    verify_netlist_classes(old.netlist(), new.netlist())?;
    let classes = source_netclasses(new.netlist())?;
    let class_for = |name: &str| -> Result<Option<String>, String> {
        classes
            .get(name)
            .map(|value| value.class.clone())
            .ok_or("source pin has no complete native class identity".into())
    };
    let mut pins = old.pins().pins().to_vec();
    for pin in &mut pins {
        pin.net.class = class_for(&pin.net.name)?;
    }
    let mut nets = before.connectivity.connectivity().nets().to_vec();
    for net in &mut nets {
        // Private contact witnesses without an exported native name remain fully immutable.
        if let Some(value) = classes.get(&net.name) {
            net.class = value.class.clone();
        }
    }
    let mut components = before.components().to_vec();
    for component in &mut components {
        for pin in component.pins.values_mut() {
            pin.class = class_for(&pin.name)?;
        }
    }
    if old.inner.parsed != new.inner.parsed
        || old.identities() != new.identities()
        || old.sheet_inventory()? != new.sheet_inventory()?
        || old.pins().unexported() != new.pins().unexported()
        || pins != new.pins().pins()
        || nets != after.connectivity.connectivity().nets()
        || before.connectivity.connectivity().isolated_no_connect()
            != after.connectivity.connectivity().isolated_no_connect()
        || components != after.components()
        || before.ineligible_units() != after.ineligible_units()
    {
        return Err(
            "project candidate changes protected schematic hierarchy/pins/connectivity/ERC".into(),
        );
    }
    for identity in old.identities().components() {
        let mut units = identity
            .units
            .iter()
            .map(SourceUnitKey::from)
            .collect::<Vec<_>>();
        units.sort();
        if old.pins().component_pin_numbers(&units) != new.pins().component_pin_numbers(&units) {
            return Err("project candidate changes complete component pin inventory".into());
        }
    }
    Ok(changed_records(
        &source_netclasses(old.netlist())?,
        &classes,
    ))
}

pub(super) fn verify_source_classes(
    before: &SynchronizationSource,
    after: &SynchronizationSource,
) -> Result<BTreeMap<String, NetclassRecordChange<SourceNetclass>>, String> {
    let records = verify_source_class_records(before, after)?;
    if !before
        .audit
        .erc()
        .equivalent_to(after.audit.erc())
        .map_err(|e| e.to_string())?
    {
        return Err("project source ERC differs from its matched candidate".into());
    }
    Ok(records)
}

pub(super) fn verify_defined_source_classes(
    before: &SynchronizationSource,
    after: &SynchronizationSource,
    original_project: &[u8],
    actions: &[ProjectNetclassChange],
) -> Result<
    (
        BTreeMap<String, NetclassRecordChange<SourceNetclass>>,
        kicad_checks::erc::ErcDelta,
    ),
    String,
> {
    let records = verify_source_class_records(before, after)?;
    let erc = crate::schematic_audit::synchronization::source_classes::definition_erc(
        &before.audit,
        &after.audit,
        original_project,
        actions,
    )?;
    Ok((records, erc))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_class_projection_cannot_change_net_codes_membership_or_other_native_fields() {
        let before = NetlistDocument {
            export_version: "E".into(),
            sheets: vec![],
            components: vec![],
            nets: vec![kicad_schematic::NetlistNet {
                code: "1".into(),
                name: "N".into(),
                class: Some("Default".into()),
                nodes: vec![],
            }],
        };
        let mut after = before.clone();
        after.nets[0].class = Some("Power,Default".into());
        verify_netlist_classes(&before, &after).unwrap();
        after.nets[0].code = "2".into();
        assert!(verify_netlist_classes(&before, &after).is_err());
        after.nets[0].code = "1".into();
        after.nets[0].name = "foreign".into();
        assert!(verify_netlist_classes(&before, &after).is_err());
        after = before.clone();
        after.nets.push(after.nets[0].clone());
        assert!(verify_netlist_classes(&before, &after).is_err());
        after = before.clone();
        after.export_version = "future".into();
        assert!(verify_netlist_classes(&before, &after).is_err());
    }
}
