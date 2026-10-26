use super::*;

fn invalid() -> IpcError {
    IpcError::InvalidObservedFootprint("unsupported or incomplete footprint flip geometry")
}

fn layer(value: &mut protobuf::EnumOrUnknown<board_types::BoardLayer>) -> Result<(), IpcError> {
    use board_types::BoardLayer::*;
    *value = protobuf::EnumOrUnknown::new(match value.enum_value().map_err(|_| invalid())? {
        BL_F_Cu => BL_B_Cu,
        BL_B_Cu => BL_F_Cu,
        BL_F_Adhes => BL_B_Adhes,
        BL_B_Adhes => BL_F_Adhes,
        BL_F_Paste => BL_B_Paste,
        BL_B_Paste => BL_F_Paste,
        BL_F_Mask => BL_B_Mask,
        BL_B_Mask => BL_F_Mask,
        BL_F_SilkS => BL_B_SilkS,
        BL_B_SilkS => BL_F_SilkS,
        BL_F_Fab => BL_B_Fab,
        BL_B_Fab => BL_F_Fab,
        BL_F_CrtYd => BL_B_CrtYd,
        BL_B_CrtYd => BL_F_CrtYd,
        v @ (BL_Dwgs_User | BL_Cmts_User | BL_Eco1_User | BL_Eco2_User | BL_Edge_Cuts
        | BL_Margin) => v,
        _ => return Err(invalid()),
    });
    Ok(())
}

struct Reflection {
    origin: base_types::Vector2,
    axis: FootprintFlipAxis,
}

impl Reflection {
    fn point(&self, point: &mut MessageField<base_types::Vector2>) -> Result<(), IpcError> {
        let point = point.as_mut().ok_or_else(invalid)?;
        let (value, origin) = match self.axis {
            FootprintFlipAxis::TopBottom => (&mut point.y_nm, self.origin.y_nm),
            FootprintFlipAxis::LeftRight => (&mut point.x_nm, self.origin.x_nm),
        };
        *value = origin
            .checked_mul(2)
            .and_then(|n| n.checked_sub(*value))
            .ok_or_else(invalid)?;
        if point.x_nm.unsigned_abs() > 2_000_000_000 || point.y_nm.unsigned_abs() > 2_000_000_000 {
            return Err(invalid());
        }
        Ok(())
    }

    fn text(&self, value: &mut board_types::BoardText) -> Result<(), IpcError> {
        if footprint_transform::is_copper(value.layer)? {
            return Err(invalid());
        }
        let old_layer = value.layer;
        layer(&mut value.layer)?;
        let text = value.text.as_mut().ok_or_else(invalid)?;
        self.point(&mut text.position)?;
        let attributes = text.attributes.as_mut().ok_or_else(invalid)?;
        let angle = attributes.angle.as_mut().ok_or_else(invalid)?;
        if !angle.value_degrees.is_finite() {
            return Err(invalid());
        }
        angle.value_degrees = match self.axis {
            FootprintFlipAxis::TopBottom => 180.0 - angle.value_degrees,
            FootprintFlipAxis::LeftRight => -angle.value_degrees,
        }
        .rem_euclid(360.0);
        if old_layer != value.layer {
            attributes.mirrored = !attributes.mirrored;
        }
        if self.axis == FootprintFlipAxis::LeftRight
            && attributes.keep_upright
            && angle.value_degrees >= 180.0
        {
            angle.value_degrees -= 180.0;
            use enums::{HorizontalAlignment as H, VerticalAlignment as V};
            attributes.horizontal_alignment = protobuf::EnumOrUnknown::new(
                match attributes
                    .horizontal_alignment
                    .enum_value()
                    .map_err(|_| invalid())?
                {
                    H::HA_LEFT => H::HA_RIGHT,
                    H::HA_RIGHT => H::HA_LEFT,
                    H::HA_CENTER => H::HA_CENTER,
                    _ => return Err(invalid()),
                },
            );
            attributes.vertical_alignment = protobuf::EnumOrUnknown::new(
                match attributes
                    .vertical_alignment
                    .enum_value()
                    .map_err(|_| invalid())?
                {
                    V::VA_TOP => V::VA_BOTTOM,
                    V::VA_BOTTOM => V::VA_TOP,
                    V::VA_CENTER => V::VA_CENTER,
                    _ => return Err(invalid()),
                },
            );
        }
        Ok(())
    }

    fn field(&self, value: &mut board_types::Field) -> Result<(), IpcError> {
        self.text(value.text.as_mut().ok_or_else(invalid)?)
    }

    fn shape(&self, value: &mut board_types::BoardGraphicShape) -> Result<(), IpcError> {
        if footprint_transform::is_copper(value.layer)? {
            return Err(invalid());
        }
        layer(&mut value.layer)?;
        use base_types::graphic_shape::Geometry;
        match value
            .shape
            .as_mut()
            .ok_or_else(invalid)?
            .geometry
            .as_mut()
            .ok_or_else(invalid)?
        {
            Geometry::Segment(g) => {
                self.point(&mut g.start)?;
                self.point(&mut g.end)?;
            }
            Geometry::Circle(g) => {
                self.point(&mut g.center)?;
                self.point(&mut g.radius_point)?;
            }
            Geometry::Arc(g) => {
                self.point(&mut g.start)?;
                self.point(&mut g.mid)?;
                self.point(&mut g.end)?;
                std::mem::swap(&mut g.start, &mut g.end);
            }
            Geometry::Rectangle(g) => {
                self.point(&mut g.top_left)?;
                self.point(&mut g.bottom_right)?;
                let a = g.top_left.as_mut().unwrap();
                let b = g.bottom_right.as_mut().unwrap();
                if a.x_nm > b.x_nm {
                    std::mem::swap(&mut a.x_nm, &mut b.x_nm);
                }
                if a.y_nm > b.y_nm {
                    std::mem::swap(&mut a.y_nm, &mut b.y_nm);
                }
            }
            Geometry::Bezier(g) => {
                self.point(&mut g.start)?;
                self.point(&mut g.control1)?;
                self.point(&mut g.control2)?;
                self.point(&mut g.end)?;
            }
            Geometry::Polygon(_) => return Err(invalid()),
        }
        Ok(())
    }

    fn pad(&self, value: &mut board_types::Pad) -> Result<(), IpcError> {
        self.point(&mut value.position)?;
        let stack = value.pad_stack.as_mut().ok_or_else(invalid)?;
        // Normal stacks use the front-copper entry as an all-layer shape definition.
        if stack.type_.enum_value() != Ok(board_types::PadStackType::PST_NORMAL) {
            return Err(invalid());
        }
        let angle = stack.angle.as_mut().ok_or_else(invalid)?;
        if !angle.value_degrees.is_finite() {
            return Err(invalid());
        }
        angle.value_degrees = match self.axis {
            FootprintFlipAxis::TopBottom => -angle.value_degrees,
            FootprintFlipAxis::LeftRight => 180.0 - angle.value_degrees,
        }
        .rem_euclid(360.0);
        for item in &mut stack.layers {
            layer(item)?;
        }
        stack.layers.sort_by_key(|v| v.value());
        for shape in &mut stack.copper_layers {
            if !shape.custom_shapes.is_empty()
                || shape.layer.enum_value() != Ok(board_types::BoardLayer::BL_F_Cu)
            {
                return Err(invalid());
            }
            if let Some(offset) = shape.offset.as_mut() {
                offset.y_nm = offset.y_nm.checked_neg().ok_or_else(invalid)?;
            }
            if let Some(delta) = shape.trapezoid_delta.as_mut() {
                delta.y_nm = delta.y_nm.checked_neg().ok_or_else(invalid)?;
            }
            if let Some(corners) = shape.chamfered_corners.as_mut() {
                std::mem::swap(&mut corners.top_left, &mut corners.bottom_left);
                std::mem::swap(&mut corners.top_right, &mut corners.bottom_right);
            }
        }
        for drill in [
            &mut stack.drill,
            &mut stack.secondary_drill,
            &mut stack.tertiary_drill,
        ] {
            if let Some(drill) = drill.as_mut() {
                layer(&mut drill.start_layer)?;
                layer(&mut drill.end_layer)?;
            }
        }
        std::mem::swap(&mut stack.front_outer_layers, &mut stack.back_outer_layers);
        std::mem::swap(
            &mut stack.front_post_machining,
            &mut stack.back_post_machining,
        );
        Ok(())
    }
}

fn edit<M: Message>(
    item: &mut Any,
    apply: impl FnOnce(&mut M) -> Result<(), IpcError>,
) -> Result<(), IpcError> {
    let mut value = M::parse_from_bytes(&item.value).map_err(|_| IpcError::MalformedEnvelope)?;
    apply(&mut value)?;
    item.value = value
        .write_to_bytes()
        .map_err(|_| IpcError::MalformedEnvelope)?;
    Ok(())
}

pub(super) fn flip(
    value: &mut board_types::FootprintInstance,
    axis: FootprintFlipAxis,
) -> Result<(), IpcError> {
    if value.locked.enum_value() != Ok(base_types::LockedState::LS_UNLOCKED) {
        return Err(invalid());
    }
    let mirror = Reflection {
        origin: value.position.as_ref().ok_or_else(invalid)?.clone(),
        axis,
    };
    layer(&mut value.layer)?;
    let angle = value.orientation.as_mut().ok_or_else(invalid)?;
    if !angle.value_degrees.is_finite() {
        return Err(invalid());
    }
    angle.value_degrees = match axis {
        FootprintFlipAxis::TopBottom => -angle.value_degrees,
        FootprintFlipAxis::LeftRight => 180.0 - angle.value_degrees,
    }
    .rem_euclid(360.0);
    for field in [
        &mut value.reference_field,
        &mut value.value_field,
        &mut value.datasheet_field,
        &mut value.description_field,
    ] {
        if let Some(field) = field.as_mut() {
            mirror.field(field)?;
        }
    }
    for item in &mut value.definition.as_mut().ok_or_else(invalid)?.items {
        match item.type_url.as_str() {
            PAD_TYPE_URL => edit::<board_types::Pad>(item, |v| mirror.pad(v))?,
            "type.googleapis.com/kiapi.board.types.Field" => {
                edit::<board_types::Field>(item, |v| mirror.field(v))?
            }
            "type.googleapis.com/kiapi.board.types.BoardText" => {
                edit::<board_types::BoardText>(item, |v| mirror.text(v))?
            }
            "type.googleapis.com/kiapi.board.types.BoardGraphicShape" => {
                edit::<board_types::BoardGraphicShape>(item, |v| mirror.shape(v))?
            }
            "type.googleapis.com/kiapi.board.types.Footprint3DModel" => {}
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

impl FootprintInstanceTemplate {
    /// Compare a derived flip result, canonicalizing cyclic angles and layer-set ordering.
    pub fn matches_flip_result(&self, after: &Self) -> Result<bool, IpcError> {
        fn canonical(
            template: &FootprintInstanceTemplate,
        ) -> Result<board_types::FootprintInstance, IpcError> {
            let mut value = board_types::FootprintInstance::parse_from_bytes(&template.bytes)
                .map_err(|_| IpcError::MalformedEnvelope)?;
            fn angle(value: &mut MessageField<base_types::Angle>) -> Result<(), IpcError> {
                let value = value.as_mut().ok_or_else(invalid)?;
                if !value.value_degrees.is_finite() {
                    return Err(invalid());
                }
                value.value_degrees = value.value_degrees.rem_euclid(360.0);
                Ok(())
            }
            fn text(value: &mut board_types::BoardText) -> Result<(), IpcError> {
                angle(
                    &mut value
                        .text
                        .as_mut()
                        .ok_or_else(invalid)?
                        .attributes
                        .as_mut()
                        .ok_or_else(invalid)?
                        .angle,
                )
            }
            fn field(value: &mut board_types::Field) -> Result<(), IpcError> {
                text(value.text.as_mut().ok_or_else(invalid)?)
            }
            angle(&mut value.orientation)?;
            for f in [
                &mut value.reference_field,
                &mut value.value_field,
                &mut value.datasheet_field,
                &mut value.description_field,
            ] {
                if let Some(f) = f.as_mut() {
                    field(f)?;
                }
            }
            for item in &mut value.definition.as_mut().ok_or_else(invalid)?.items {
                match item.type_url.as_str() {
                    PAD_TYPE_URL => edit::<board_types::Pad>(item, |pad| {
                        let stack = pad.pad_stack.as_mut().ok_or_else(invalid)?;
                        angle(&mut stack.angle)?;
                        stack.layers.sort_by_key(|v| v.value());
                        if let Some(drill) = stack.drill.as_mut() {
                            use board_types::BoardLayer::{BL_B_Cu, BL_F_Cu};
                            if drill.start_layer.enum_value() == Ok(BL_B_Cu)
                                && drill.end_layer.enum_value() == Ok(BL_F_Cu)
                            {
                                std::mem::swap(&mut drill.start_layer, &mut drill.end_layer);
                            }
                        }
                        Ok(())
                    })?,
                    "type.googleapis.com/kiapi.board.types.Field" => {
                        edit::<board_types::Field>(item, field)?
                    }
                    "type.googleapis.com/kiapi.board.types.BoardText" => {
                        edit::<board_types::BoardText>(item, text)?
                    }
                    _ => {}
                }
            }
            value.parent = MessageField::none();
            Ok(value)
        }
        Ok(canonical(self)? == canonical(after)?)
    }

    /// Derive a native footprint reflection without consulting the result of FlipItems.
    pub fn expected_flipped(&self, axis: FootprintFlipAxis) -> Result<Self, IpcError> {
        let mut value = board_types::FootprintInstance::parse_from_bytes(&self.bytes)
            .map_err(|_| IpcError::MalformedEnvelope)?;
        flip(&mut value, axis)?;
        let mut result = self.clone();
        result.bytes = value
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?;
        result.metadata.orientation_mdeg =
            (value.orientation.as_ref().unwrap().value_degrees * 1000.0).round() as i64;
        result.metadata.side = match self.metadata.side {
            FootprintSide::Front => FootprintSide::Back,
            FootprintSide::Back => FootprintSide::Front,
        };
        Ok(result)
    }
}
