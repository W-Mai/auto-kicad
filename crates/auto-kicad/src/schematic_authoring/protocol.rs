//! Strict public intent types. Catalog definitions and compatibility evidence are server-owned.
use kicad_adapter::schematic_audit::{
    ConnectivityExpectation, ErcExpectation, NoConnectMarker, SchematicAudit,
    SchematicEditExpectation,
};
use kicad_checks::erc::ErcDiagnostic;
use kicad_schematic::{
    editing::*,
    geometry::{Mirror, Point, QuarterTurn, SymbolTransform},
    pins::{LogicalPinKey, SourceUnitKey},
    probes::{ConnectivityNet, ConnectivityPin},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObserveRequest {
    pub project_root: String,
    pub root_schematic: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ReleaseRequest {
    pub observation_ids: Vec<String>,
    pub plan_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneralRequest {
    pub schema_version: u32,
    pub observation_id: String,
    pub observed_revision: String,
    pub edits: Vec<EditRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointRequest {
    pub x_nm: i64,
    pub y_nm: i64,
}
impl PointRequest {
    pub fn point(&self) -> Result<Point, String> {
        Point::from_nm(self.x_nm, self.y_nm)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Zero,
    Ccw90,
    Half,
    Ccw270,
}
impl Rotation {
    fn native(self) -> QuarterTurn {
        match self {
            Self::Zero => QuarterTurn::Zero,
            Self::Ccw90 => QuarterTurn::Counterclockwise90,
            Self::Half => QuarterTurn::Half,
            Self::Ccw270 => QuarterTurn::Counterclockwise270,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MirrorRequest {
    #[default]
    None,
    X,
    Y,
}
impl MirrorRequest {
    fn native(self) -> Mirror {
        match self {
            Self::None => Mirror::None,
            Self::X => Mirror::X,
            Self::Y => Mirror::Y,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformRequest {
    pub position: PointRequest,
    #[serde(default)]
    pub rotation: Rotation,
    #[serde(default)]
    pub mirror: MirrorRequest,
}
impl TransformRequest {
    fn native(&self) -> Result<SymbolTransform, String> {
        Ok(SymbolTransform {
            origin: self.position.point()?,
            rotation: self.rotation.native(),
            mirror: self.mirror.native(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScopeRequest {
    FileAllInstances,
    SingleInstance { path: String },
}
impl ScopeRequest {
    fn native(&self) -> EditScope {
        match self {
            Self::FileAllInstances => EditScope::FileAllInstances,
            Self::SingleInstance { path } => EditScope::SingleInstance(path.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Symbol,
    Wire,
    Junction,
    NoConnect,
    LocalLabel,
    GlobalLabel,
    HierarchicalLabel,
}
impl Kind {
    fn native(self) -> ObjectKind {
        match self {
            Self::Symbol => ObjectKind::Symbol,
            Self::Wire => ObjectKind::Wire,
            Self::Junction => ObjectKind::Junction,
            Self::NoConnect => ObjectKind::NoConnect,
            Self::LocalLabel => ObjectKind::LocalLabel,
            Self::GlobalLabel => ObjectKind::GlobalLabel,
            Self::HierarchicalLabel => ObjectKind::HierarchicalLabel,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Input,
    Output,
    Bidirectional,
    TriState,
    Passive,
}
impl Shape {
    fn native(self) -> ElectricalShape {
        match self {
            Self::Input => ElectricalShape::Input,
            Self::Output => ElectricalShape::Output,
            Self::Bidirectional => ElectricalShape::Bidirectional,
            Self::TriState => ElectricalShape::TriState,
            Self::Passive => ElectricalShape::Passive,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LabelKindRequest {
    Local,
    Global { shape: Shape },
    Hierarchical { shape: Shape },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelRequest {
    pub label: LabelKindRequest,
    pub text: String,
    pub position: PointRequest,
    #[serde(default)]
    pub rotation: Rotation,
}
impl LabelRequest {
    fn native(&self) -> Result<Label, String> {
        Ok(Label {
            kind: match self.label {
                LabelKindRequest::Local => LabelKind::Local,
                LabelKindRequest::Global { shape } => LabelKind::Global(shape.native()),
                LabelKindRequest::Hierarchical { shape } => LabelKind::Hierarchical(shape.native()),
            },
            text: self.text.clone(),
            position: self.position.point()?,
            rotation: self.rotation.native(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Flags {
    pub in_bom: Option<bool>,
    pub on_board: Option<bool>,
    pub dnp: Option<bool>,
}
impl Flags {
    fn native(&self) -> SymbolFlags {
        SymbolFlags {
            in_bom: self.in_bom,
            on_board: self.on_board,
            dnp: self.dnp,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SheetRectangleRequest {
    pub position: PointRequest,
    pub width_nm: i64,
    pub height_nm: i64,
}
impl SheetRectangleRequest {
    fn native(&self) -> Result<SheetRectangle, String> {
        Ok(SheetRectangle {
            position: self.position.point()?,
            width: kicad_schematic::geometry::Coordinate::from_nm(self.width_nm)?,
            height: kicad_schematic::geometry::Coordinate::from_nm(self.height_nm)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SheetPinRequest {
    pub name: String,
    pub shape: Shape,
    pub position: PointRequest,
    pub rotation: Rotation,
}
impl SheetPinRequest {
    fn native(&self) -> Result<SheetPin, String> {
        Ok(SheetPin {
            name: self.name.clone(),
            shape: self.shape.native(),
            position: self.position.point()?,
            rotation: self.rotation.native(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SheetPinEditRequest {
    Create { key: String, pin: SheetPinRequest },
    Update { uuid: String, pin: SheetPinRequest },
    Delete { uuid: String },
}
impl SheetPinEditRequest {
    fn native(&self) -> Result<SheetPinEdit, String> {
        Ok(match self {
            Self::Create { key, pin } => SheetPinEdit::Create {
                key: key.clone(),
                pin: pin.native()?,
            },
            Self::Update { uuid, pin } => SheetPinEdit::Update {
                uuid: uuid.clone(),
                pin: pin.native()?,
            },
            Self::Delete { uuid } => SheetPinEdit::Delete { uuid: uuid.clone() },
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SheetUpdateRequest {
    pub name: Option<String>,
    pub rectangle: Option<SheetRectangleRequest>,
    pub pins: Vec<SheetPinEditRequest>,
}
impl SheetUpdateRequest {
    fn native(&self) -> Result<SheetUpdate, String> {
        Ok(SheetUpdate {
            name: self.name.clone(),
            rectangle: self
                .rectangle
                .as_ref()
                .map(SheetRectangleRequest::native)
                .transpose()?,
            pins: self
                .pins
                .iter()
                .map(SheetPinEditRequest::native)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub file: String,
    pub scope: ScopeRequest,
    pub action: Action,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    RemoveSheet {
        uuid: String,
    },
    UpdateSheet {
        uuid: String,
        update: SheetUpdateRequest,
    },
    SetProperty {
        uuid: String,
        name: String,
        value: String,
    },
    SetReference {
        uuid: String,
        reference: String,
    },
    SetFlags {
        uuid: String,
        flags: Flags,
    },
    TransformSymbol {
        uuid: String,
        transform: TransformRequest,
    },
    AssignFootprint {
        uuid: String,
        footprint: String,
        #[serde(default)]
        pin_pad_map: BTreeMap<String, String>,
    },
    CreateSymbol {
        key: String,
        lib_id: String,
        reference: String,
        value: String,
        unit: u32,
        body_style: Option<u32>,
        transform: TransformRequest,
        #[serde(default)]
        flags: Flags,
        #[serde(default)]
        instance_references: BTreeMap<String, String>,
    },
    AddSymbolUnit {
        key: String,
        uuid: String,
        unit: u32,
        transform: TransformRequest,
    },
    CreateWire {
        key: String,
        start: PointRequest,
        end: PointRequest,
    },
    UpdateWire {
        uuid: String,
        start: PointRequest,
        end: PointRequest,
    },
    CreateJunction {
        key: String,
        position: PointRequest,
    },
    CreateNoConnect {
        key: String,
        position: PointRequest,
    },
    MoveMarker {
        uuid: String,
        object_kind: Kind,
        position: PointRequest,
    },
    CreateLabel {
        key: String,
        label: LabelRequest,
    },
    UpdateLabel {
        uuid: String,
        label: LabelRequest,
    },
    Delete {
        uuid: String,
        object_kind: Kind,
    },
}
impl EditRequest {
    pub fn resolve(&self, audit: &SchematicAudit) -> Result<SchematicEdit, String> {
        if let Action::CreateSymbol {
            instance_references,
            ..
        } = &self.action
            && !instance_references.is_empty()
        {
            let expected = audit
                .identities()
                .sheets()
                .iter()
                .filter(|sheet| sheet.file == self.file)
                .map(|sheet| sheet.path.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            if instance_references
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                != expected
            {
                return Err(
                    "instance reference map must exactly cover the observed physical sheet".into(),
                );
            }
        }
        let action = match &self.action {
            Action::RemoveSheet { uuid } => EditAction::RemoveSheet { uuid: uuid.clone() },
            Action::UpdateSheet { uuid, update } => EditAction::UpdateSheet {
                uuid: uuid.clone(),
                update: update.native()?,
            },
            Action::SetProperty { uuid, name, value } => EditAction::SetProperty {
                uuid: uuid.clone(),
                property: if name == "Value" {
                    SymbolProperty::Value
                } else {
                    SymbolProperty::Named(name.clone())
                },
                value: value.clone(),
            },
            Action::SetReference { uuid, reference } => EditAction::SetReference {
                uuid: uuid.clone(),
                reference: reference.clone(),
                instance_path: match &self.scope {
                    ScopeRequest::SingleInstance { path } => Some(path.clone()),
                    _ => None,
                },
            },
            Action::SetFlags { uuid, flags } => EditAction::SetFlags {
                uuid: uuid.clone(),
                flags: flags.native(),
            },
            Action::TransformSymbol { uuid, transform } => EditAction::TransformSymbol {
                uuid: uuid.clone(),
                transform: transform.native()?,
            },
            Action::AssignFootprint {
                uuid,
                footprint,
                pin_pad_map,
            } => EditAction::AssignFootprint {
                uuid: uuid.clone(),
                footprint: footprint.clone(),
                evidence: audit.footprint_assignment_evidence(
                    &self.file,
                    uuid,
                    footprint,
                    pin_pad_map.clone(),
                )?,
            },
            Action::CreateSymbol {
                key,
                lib_id,
                reference,
                value,
                unit,
                body_style,
                transform,
                flags,
                instance_references,
            } => EditAction::CreateSymbol {
                key: key.clone(),
                symbol: SymbolCreation {
                    evidence: audit.symbol_definition(lib_id)?,
                    transform: transform.native()?,
                    unit: *unit,
                    body_style: *body_style,
                    reference: reference.clone(),
                    value: value.clone(),
                    footprint: None,
                    flags: flags.native(),
                    instance_paths: audit
                        .identities()
                        .sheets()
                        .iter()
                        .filter(|sheet| sheet.file == self.file)
                        .map(|sheet| SymbolInstancePath {
                            path: sheet.path.clone(),
                            reference: instance_references.get(&sheet.path).cloned(),
                        })
                        .collect(),
                },
            },
            Action::AddSymbolUnit {
                key,
                uuid,
                unit,
                transform,
            } => EditAction::AddSymbolUnit {
                key: key.clone(),
                uuid: uuid.clone(),
                unit: *unit,
                transform: transform.native()?,
            },
            Action::CreateWire { key, start, end } => EditAction::Create {
                key: key.clone(),
                object: NewObject::Wire {
                    start: start.point()?,
                    end: end.point()?,
                },
            },
            Action::UpdateWire { uuid, start, end } => EditAction::UpdateWire {
                uuid: uuid.clone(),
                start: start.point()?,
                end: end.point()?,
            },
            Action::CreateJunction { key, position } => EditAction::Create {
                key: key.clone(),
                object: NewObject::Junction(position.point()?),
            },
            Action::CreateNoConnect { key, position } => EditAction::Create {
                key: key.clone(),
                object: NewObject::NoConnect(position.point()?),
            },
            Action::MoveMarker {
                uuid,
                object_kind,
                position,
            } => EditAction::MoveMarker {
                uuid: uuid.clone(),
                kind: object_kind.native(),
                position: position.point()?,
            },
            Action::CreateLabel { key, label } => EditAction::Create {
                key: key.clone(),
                object: NewObject::Label(label.native()?),
            },
            Action::UpdateLabel { uuid, label } => EditAction::UpdateLabel {
                uuid: uuid.clone(),
                label: label.native()?,
            },
            Action::Delete { uuid, object_kind } => EditAction::Delete {
                uuid: uuid.clone(),
                kind: object_kind.native(),
            },
        };
        Ok(SchematicEdit {
            file: self.file.clone(),
            scope: self.scope.native(),
            action,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitKey {
    pub file: String,
    pub sheet_path: String,
    pub symbol_uuid: String,
}
impl UnitKey {
    fn native(&self) -> SourceUnitKey {
        SourceUnitKey {
            file: self.file.clone(),
            sheet_path: self.sheet_path.clone(),
            symbol_uuid: self.symbol_uuid.clone(),
        }
    }
}
impl From<&SourceUnitKey> for UnitKey {
    fn from(unit: &SourceUnitKey) -> Self {
        Self {
            file: unit.file.clone(),
            sheet_path: unit.sheet_path.clone(),
            symbol_uuid: unit.symbol_uuid.clone(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pin {
    Component { units: Vec<UnitKey>, number: String },
    Virtual { source: UnitKey, number: String },
}
impl Pin {
    fn native(&self) -> ConnectivityPin {
        match self {
            Self::Component { units, number } => ConnectivityPin::Component(LogicalPinKey {
                component_units: units.iter().map(UnitKey::native).collect::<Vec<_>>().into(),
                number: number.clone(),
            }),
            Self::Virtual { source, number } => ConnectivityPin::Virtual {
                source: source.native(),
                number: number.clone(),
            },
        }
    }
}
impl From<&ConnectivityPin> for Pin {
    fn from(pin: &ConnectivityPin) -> Self {
        match pin {
            ConnectivityPin::Component(key) => Self::Component {
                units: key.component_units.iter().map(UnitKey::from).collect(),
                number: key.number.clone(),
            },
            ConnectivityPin::Virtual { source, number } => Self::Virtual {
                source: source.into(),
                number: number.clone(),
            },
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Net {
    pub name: String,
    pub class: Option<String>,
    pub pins: Vec<Pin>,
}
impl Net {
    fn native(&self) -> ConnectivityNet {
        ConnectivityNet {
            name: self.name.clone(),
            class: self.class.clone(),
            pins: self.pins.iter().map(Pin::native).collect(),
        }
    }
}
impl From<&ConnectivityNet> for Net {
    fn from(net: &ConnectivityNet) -> Self {
        Self {
            name: net.name.clone(),
            class: net.class.clone(),
            pins: net.pins.iter().map(Pin::from).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Connectivity {
    pub removed_nets: Vec<Net>,
    pub added_nets: Vec<Net>,
    pub removed_isolated: Vec<Pin>,
    pub added_isolated: Vec<Pin>,
    pub removed_pins: Vec<Pin>,
    pub added_pins: Vec<Pin>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub file: String,
    pub uuid: String,
    pub position: PointRequest,
    pub definition_digest: String,
}
impl Marker {
    fn native(&self) -> Result<NoConnectMarker, String> {
        Ok(NoConnectMarker {
            file: self.file.clone(),
            uuid: self.uuid.clone(),
            position: self.position.point()?,
            definition_digest: self.definition_digest.clone(),
        })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub sheet_path: String,
    pub sheet_uuid_path: String,
    pub diagnostic_json: String,
}
impl Diagnostic {
    fn native(&self) -> ErcDiagnostic {
        ErcDiagnostic {
            sheet_path: self.sheet_path.clone(),
            sheet_uuid_path: self.sheet_uuid_path.clone(),
            diagnostic_json: self.diagnostic_json.clone(),
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Erc {
    pub removed_sheet_headers: Vec<String>,
    pub added_sheet_headers: Vec<String>,
    pub introduced: Vec<Diagnostic>,
    pub resolved: Vec<Diagnostic>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Expectations {
    pub connectivity: Connectivity,
    pub removed_markers: Vec<Marker>,
    pub added_markers: Vec<Marker>,
    pub erc: Erc,
}
impl Expectations {
    pub fn native(&self) -> Result<SchematicEditExpectation, String> {
        let pins = |values: &[Pin]| values.iter().map(Pin::native).collect();
        Ok(SchematicEditExpectation {
            connectivity: ConnectivityExpectation {
                removed_nets: self
                    .connectivity
                    .removed_nets
                    .iter()
                    .map(Net::native)
                    .collect(),
                added_nets: self
                    .connectivity
                    .added_nets
                    .iter()
                    .map(Net::native)
                    .collect(),
                removed_isolated: pins(&self.connectivity.removed_isolated),
                added_isolated: pins(&self.connectivity.added_isolated),
                removed_pins: pins(&self.connectivity.removed_pins),
                added_pins: pins(&self.connectivity.added_pins),
            },
            removed_markers: self
                .removed_markers
                .iter()
                .map(Marker::native)
                .collect::<Result<_, _>>()?,
            added_markers: self
                .added_markers
                .iter()
                .map(Marker::native)
                .collect::<Result<_, _>>()?,
            erc: ErcExpectation {
                removed_sheet_headers: self.erc.removed_sheet_headers.clone(),
                added_sheet_headers: self.erc.added_sheet_headers.clone(),
                introduced: self.erc.introduced.iter().map(Diagnostic::native).collect(),
                resolved: self.erc.resolved.iter().map(Diagnostic::native).collect(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_intent_rejects_resource_authority_forgery_and_off_grid_points() {
        for text in [
            r#"{"kind":"create_symbol","key":"x","lib_id":"Device:R","reference":"R1","value":"1k","unit":1,"body_style":null,"transform":{"position":{"x_nm":0,"y_nm":0}},"evidence":{}}"#,
            r#"{"kind":"assign_footprint","uuid":"x","footprint":"a:b","catalog_digest":"fake"}"#,
        ] {
            assert!(serde_json::from_str::<Action>(text).is_err());
        }
        assert!(PointRequest { x_nm: 1, y_nm: 0 }.point().is_err());
        assert!(
            PointRequest {
                x_nm: i64::MAX,
                y_nm: 0
            }
            .point()
            .is_err()
        );
        assert!(serde_json::from_str::<Expectations>(r#"{"ignore_warnings":true}"#).is_err());
    }
}
