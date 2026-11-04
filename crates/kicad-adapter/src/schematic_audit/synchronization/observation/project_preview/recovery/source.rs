//! Exhaustive stable native netlist projection for versioned restart evidence.
use kicad_schematic::{
    NetlistComponent, NetlistDocument, NetlistNet, NetlistNode, NetlistSheet, NetlistUnit,
};
use serde_json::{Value, json};

pub(super) fn netlist(document: &NetlistDocument) -> Value {
    let NetlistDocument {
        export_version,
        sheets,
        components,
        nets,
    } = document;
    json!({"export_version":export_version,
        "sheets":sheets.iter().map(sheet).collect::<Vec<_>>(),
        "components":components.iter().map(component).collect::<Vec<_>>(),
        "nets":nets.iter().map(net).collect::<Vec<_>>()})
}
fn sheet(value: &NetlistSheet) -> Value {
    let NetlistSheet {
        number,
        name,
        uuid_path,
    } = value;
    json!({"number":number,"name":name,"uuid_path":uuid_path})
}
fn component(value: &NetlistComponent) -> Value {
    let NetlistComponent {
        reference,
        value,
        footprint,
        symbol_uuid,
        sheet_path,
        sheet_uuid_path,
        library,
        library_part,
        units,
    } = value;
    json!({"reference":reference,"value":value,"footprint":footprint,"symbol_uuid":symbol_uuid,
        "sheet_path":sheet_path,"sheet_uuid_path":sheet_uuid_path,"library":library,"library_part":library_part,
        "units":units.as_ref().map(|units| units.iter().map(unit).collect::<Vec<_>>())})
}
fn unit(value: &NetlistUnit) -> Value {
    let NetlistUnit { name, pins } = value;
    json!({"name":name,"pins":pins})
}
fn net(value: &NetlistNet) -> Value {
    let NetlistNet {
        code,
        name,
        class,
        nodes,
    } = value;
    json!({"code":code,"name":name,"class":class,"nodes":nodes.iter().map(node).collect::<Vec<_>>()})
}
fn node(value: &NetlistNode) -> Value {
    let NetlistNode {
        reference,
        pin,
        pin_function,
        pin_type,
    } = value;
    json!({"reference":reference,"pin":pin,"pin_function":pin_function,"pin_type":pin_type})
}

use kicad_schematic::{
    geometry::Point,
    identities::{ComponentIdentity, SchematicIdentityIndex, SheetInstance, SymbolUnitIdentity},
    pins::{LogicalPin, LogicalPinKey, NativePinNet, PinOccurrence, SourceUnitKey, UnexportedPin},
};
pub(super) fn source_unit(value: &SourceUnitKey) -> Value {
    let SourceUnitKey {
        file,
        sheet_path,
        symbol_uuid,
    } = value;
    json!({"file":file,"sheet_path":sheet_path,"symbol_uuid":symbol_uuid})
}
fn identity_unit(value: &SymbolUnitIdentity) -> Value {
    let SymbolUnitIdentity {
        file,
        sheet_path,
        source_uuid,
        reference,
        unit,
    } = value;
    json!({"file":file,"sheet_path":sheet_path,"source_uuid":source_uuid,"reference":reference,"unit":unit})
}
pub(super) fn identities(value: &SchematicIdentityIndex) -> Value {
    json!({"sheets":value.sheets().iter().map(|value| {
        let SheetInstance { file, path } = value;
        json!({"file":file,"path":path})
    }).collect::<Vec<_>>(), "units":value.units().iter().map(identity_unit).collect::<Vec<_>>(),
        "components":value.components().iter().map(|value| {
            let ComponentIdentity { reference, sheet_path, units } = value;
            json!({"reference":reference,"sheet_path":sheet_path,"units":units.iter().map(identity_unit).collect::<Vec<_>>()})
        }).collect::<Vec<_>>()})
}
fn point(value: &Point) -> Value {
    let Point { x, y } = value;
    json!({"x_nm":x.nm(),"y_nm":y.nm()})
}
fn occurrence(value: &PinOccurrence) -> Value {
    let PinOccurrence {
        source,
        unit,
        graphical_number,
        stored_pin_uuids,
        name,
        electrical_type,
        library_position,
        position,
    } = value;
    json!({"source":source_unit(source),"unit":unit,"graphical_number":graphical_number,
        "stored_pin_uuids":stored_pin_uuids.as_ref(),"name":name,"electrical_type":electrical_type,
        "library_position":point(library_position),"position":point(position)})
}
pub(super) fn pin(value: &LogicalPin) -> Value {
    let LogicalPin {
        key,
        reference,
        occurrences,
        net,
    } = value;
    let LogicalPinKey {
        component_units,
        number,
    } = key;
    let NativePinNet {
        code,
        name,
        class,
        pin_function,
        electrical_type,
        explicit_no_connect,
    } = net;
    json!({"units":component_units.iter().map(source_unit).collect::<Vec<_>>(),"number":number,
        "reference":reference,"occurrences":occurrences.iter().map(occurrence).collect::<Vec<_>>(),
        "net":{"code":code,"name":name,"class":class,"pin_function":pin_function,
            "electrical_type":electrical_type,"explicit_no_connect":explicit_no_connect}})
}
pub(super) fn unexported(value: &UnexportedPin) -> Value {
    let UnexportedPin {
        reference,
        number,
        occurrence: value,
    } = value;
    json!({"reference":reference,"number":number,"occurrence":occurrence(value)})
}
