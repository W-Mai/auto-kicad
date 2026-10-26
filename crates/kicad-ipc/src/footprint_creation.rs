use super::*;
use std::collections::{BTreeMap, BTreeSet};
type Children = BTreeMap<(String, Vec<u8>), String>;

fn invalid() -> IpcError {
    IpcError::InvalidObservedFootprint(
        "created footprint native payload differs from its source template",
    )
}

impl FootprintInstanceTemplate {
    /// Compare every native field after applying only the requested creation and association.
    pub fn matches_created_instance(
        &self,
        after: &Self,
        x: i64,
        y: i64,
        reference: &str,
        value: &str,
    ) -> Result<bool, IpcError> {
        Ok(self
            .created_child_mapping(after, x, y, reference, value)?
            .is_some())
    }

    /// Prove a unique correspondence using complete native payloads after server ID assignment.
    pub fn created_child_mapping(
        &self,
        after: &Self,
        x: i64,
        y: i64,
        reference: &str,
        value: &str,
    ) -> Result<Option<BTreeMap<String, String>>, IpcError> {
        let mut expected = footprint_instance_from_template(self, x, y, reference, value)?;
        let mut actual = board_types::FootprintInstance::parse_from_bytes(&after.bytes)
            .map_err(|_| IpcError::MalformedEnvelope)?;
        validate_footprint_instance(&actual)?;
        expected.id = actual.id.clone();
        let before_ids = normalize_creation(&mut expected, &self.metadata.source_uuid)?;
        let after_ids = normalize_creation(&mut actual, &after.metadata.source_uuid)?;
        if expected != actual || before_ids.keys().ne(after_ids.keys()) {
            return Ok(None);
        }
        Ok(Some(
            before_ids
                .into_iter()
                .map(|(key, id)| (id, after_ids[&key].clone()))
                .collect(),
        ))
    }
}

fn identity(id: &mut MessageField<base_types::KIID>) -> Result<String, IpcError> {
    let id = id.as_mut().ok_or_else(invalid)?;
    if id.value.is_empty() {
        return Err(invalid());
    }
    Ok(std::mem::take(&mut id.value))
}

fn record(children: &mut Children, key: (String, Vec<u8>), id: String) -> Result<(), IpcError> {
    if children.insert(key, id).is_some() {
        return Err(invalid());
    }
    Ok(())
}

fn parent(parent: &mut MessageField<base_types::KIID>, footprint: &str) -> Result<(), IpcError> {
    if parent.as_ref().is_some_and(|id| id.value != footprint) {
        return Err(invalid());
    }
    *parent = MessageField::none();
    Ok(())
}

fn field(
    field: &mut MessageField<board_types::Field>,
    footprint: &str,
) -> Result<Option<String>, IpcError> {
    if let Some(text) = field.as_mut().and_then(|f| f.text.as_mut()) {
        parent(&mut text.parent, footprint)?;
        if text.id.is_some() {
            return Ok(Some(identity(&mut text.id)?));
        }
    }
    Ok(None)
}

fn normalize_creation(
    instance: &mut board_types::FootprintInstance,
    footprint: &str,
) -> Result<Children, IpcError> {
    let mut children = Children::new();
    instance.parent = MessageField::none();
    for (index, value) in [
        &mut instance.reference_field,
        &mut instance.value_field,
        &mut instance.datasheet_field,
        &mut instance.description_field,
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(id) = field(value, footprint)? {
            record(
                &mut children,
                (format!("instance-field-{index}"), vec![]),
                id,
            )?;
        }
    }
    let definition = instance.definition.as_mut().ok_or_else(invalid)?;
    for (index, value) in [
        &mut definition.reference_field,
        &mut definition.value_field,
        &mut definition.datasheet_field,
        &mut definition.description_field,
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(id) = field(value, footprint)? {
            record(
                &mut children,
                (format!("definition-field-{index}"), vec![]),
                id,
            )?;
        }
    }
    for item in &mut definition.items {
        let mut assigned = None;
        match item.type_url.as_str() {
            PAD_TYPE_URL => {
                let mut pad = board_types::Pad::parse_from_bytes(&item.value)
                    .map_err(|_| IpcError::MalformedEnvelope)?;
                parent(&mut pad.parent, footprint)?;
                assigned = Some(identity(&mut pad.id)?);
                if pad
                    .symbol_pin
                    .as_ref()
                    .is_some_and(|pin| *pin == board_types::SymbolPinInfo::default())
                {
                    pad.symbol_pin = MessageField::none();
                }
                // Net names bind electrical identity; native numeric net codes are deprecated.
                if let Some(net) = pad.net.as_mut() {
                    net.code = MessageField::none();
                }
                item.value = pad
                    .write_to_bytes()
                    .map_err(|_| IpcError::MalformedEnvelope)?;
            }
            "type.googleapis.com/kiapi.board.types.BoardText" => {
                let mut text = board_types::BoardText::parse_from_bytes(&item.value)
                    .map_err(|_| IpcError::MalformedEnvelope)?;
                parent(&mut text.parent, footprint)?;
                assigned = Some(identity(&mut text.id)?);
                item.value = text
                    .write_to_bytes()
                    .map_err(|_| IpcError::MalformedEnvelope)?;
            }
            "type.googleapis.com/kiapi.board.types.Field" => {
                let parsed = board_types::Field::parse_from_bytes(&item.value)
                    .map_err(|_| IpcError::MalformedEnvelope)?;
                let mut value = MessageField::some(parsed);
                assigned = field(&mut value, footprint)?;
                item.value = value
                    .as_ref()
                    .ok_or_else(invalid)?
                    .write_to_bytes()
                    .map_err(|_| IpcError::MalformedEnvelope)?;
            }
            "type.googleapis.com/kiapi.board.types.BoardGraphicShape" => {
                let mut shape = board_types::BoardGraphicShape::parse_from_bytes(&item.value)
                    .map_err(|_| IpcError::MalformedEnvelope)?;
                parent(&mut shape.parent, footprint)?;
                assigned = Some(identity(&mut shape.id)?);
                item.value = shape
                    .write_to_bytes()
                    .map_err(|_| IpcError::MalformedEnvelope)?;
            }
            _ => {}
        }
        if let Some(id) = assigned {
            record(
                &mut children,
                (item.type_url.clone(), item.value.clone()),
                id,
            )?;
        }
    }
    // Definition children form an unordered collection; preserve multiplicity and exact bytes.
    definition
        .items
        .sort_by(|a, b| (&a.type_url, &a.value).cmp(&(&b.type_url, &b.value)));
    if children.values().collect::<BTreeSet<_>>().len() != children.len() {
        return Err(invalid());
    }
    Ok(children)
}
