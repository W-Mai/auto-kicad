use super::*;

fn invalid() -> IpcError {
    IpcError::InvalidObservedFootprint("unsupported or incomplete footprint transform geometry")
}

pub(super) fn is_copper(
    layer: protobuf::EnumOrUnknown<board_types::BoardLayer>,
) -> Result<bool, IpcError> {
    let value = layer.enum_value().map_err(|_| invalid())?;
    if matches!(
        value,
        board_types::BoardLayer::BL_UNKNOWN
            | board_types::BoardLayer::BL_UNDEFINED
            | board_types::BoardLayer::BL_UNSELECTED
    ) {
        return Err(invalid());
    }
    Ok(
        (board_types::BoardLayer::BL_F_Cu as i32..=board_types::BoardLayer::BL_B_Cu as i32)
            .contains(&layer.value()),
    )
}

struct Transform {
    from: base_types::Vector2,
    to: base_types::Vector2,
    degrees: f64,
}

impl Transform {
    fn point(&self, point: &mut base_types::Vector2) -> Result<(), IpcError> {
        let bound = 2_000_000_000_i64;
        if [
            point.x_nm,
            point.y_nm,
            self.from.x_nm,
            self.from.y_nm,
            self.to.x_nm,
            self.to.y_nm,
        ]
        .iter()
        .any(|n| n.unsigned_abs() > bound as u64)
        {
            return Err(invalid());
        }
        let dx = (point.x_nm - self.from.x_nm) as f64;
        let dy = (point.y_nm - self.from.y_nm) as f64;
        let radians = self.degrees.to_radians();
        let (sin, cos) = radians.sin_cos();
        let x = (dx * cos + dy * sin).round() as i64 + self.to.x_nm;
        let y = (-dx * sin + dy * cos).round() as i64 + self.to.y_nm;
        if x.unsigned_abs() > bound as u64 || y.unsigned_abs() > bound as u64 {
            return Err(invalid());
        }
        point.x_nm = x;
        point.y_nm = y;
        Ok(())
    }

    fn position(&self, point: &mut MessageField<base_types::Vector2>) -> Result<(), IpcError> {
        self.point(point.as_mut().ok_or_else(invalid)?)
    }

    fn angle(&self, angle: &mut MessageField<base_types::Angle>) -> Result<(), IpcError> {
        let angle = angle.mut_or_insert_default();
        if !angle.value_degrees.is_finite() {
            return Err(invalid());
        }
        angle.value_degrees = (angle.value_degrees + self.degrees).rem_euclid(360.0);
        Ok(())
    }

    fn text(&self, field: &mut board_types::BoardText) -> Result<(), IpcError> {
        if is_copper(field.layer)? {
            return Err(invalid());
        }
        let text = field.text.as_mut().ok_or_else(invalid)?;
        self.position(&mut text.position)?;
        self.angle(&mut text.attributes.mut_or_insert_default().angle)
    }

    fn field(&self, field: &mut board_types::Field) -> Result<(), IpcError> {
        self.text(field.text.as_mut().ok_or_else(invalid)?)
    }

    fn shape(&self, shape: &mut base_types::GraphicShape) -> Result<(), IpcError> {
        use base_types::graphic_shape::Geometry;
        match shape.geometry.as_mut().ok_or_else(invalid)? {
            Geometry::Segment(g) => {
                self.position(&mut g.start)?;
                self.position(&mut g.end)?;
            }
            Geometry::Arc(g) => {
                self.position(&mut g.start)?;
                self.position(&mut g.mid)?;
                self.position(&mut g.end)?;
            }
            Geometry::Circle(g) => {
                self.position(&mut g.center)?;
                self.position(&mut g.radius_point)?;
            }
            Geometry::Bezier(g) => {
                self.position(&mut g.start)?;
                self.position(&mut g.control1)?;
                self.position(&mut g.control2)?;
                self.position(&mut g.end)?;
            }
            Geometry::Rectangle(g) => {
                // Arbitrary rotation needs a representation change; do not silently change shape kind.
                if self.degrees.rem_euclid(90.0).abs() > 1e-9 {
                    return Err(invalid());
                }
                self.position(&mut g.top_left)?;
                self.position(&mut g.bottom_right)?;
            }
            Geometry::Polygon(g) => {
                for polygon in &mut g.polygons {
                    self.polyline(polygon.outline.as_mut().ok_or_else(invalid)?)?;
                    for hole in &mut polygon.holes {
                        self.polyline(hole)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn polyline(&self, line: &mut base_types::PolyLine) -> Result<(), IpcError> {
        use base_types::poly_line_node::Geometry;
        for node in &mut line.nodes {
            match node.geometry.as_mut().ok_or_else(invalid)? {
                Geometry::Point(p) => self.point(p)?,
                Geometry::Arc(a) => {
                    self.position(&mut a.start)?;
                    self.position(&mut a.mid)?;
                    self.position(&mut a.end)?;
                }
            }
        }
        Ok(())
    }
}

fn edit<T: Message>(
    item: &mut Any,
    apply: impl FnOnce(&mut T) -> Result<(), IpcError>,
) -> Result<(), IpcError> {
    let mut value = T::parse_from_bytes(&item.value).map_err(|_| IpcError::MalformedEnvelope)?;
    apply(&mut value)?;
    item.value = value
        .write_to_bytes()
        .map_err(|_| IpcError::MalformedEnvelope)?;
    Ok(())
}

pub(super) fn transform(
    instance: &mut board_types::FootprintInstance,
    x: i64,
    y: i64,
    angle_mdeg: i64,
) -> Result<(), IpcError> {
    if !(0..360_000).contains(&angle_mdeg) {
        return Err(invalid());
    }
    let original_angle = instance
        .orientation
        .as_ref()
        .ok_or_else(invalid)?
        .value_degrees;
    if !original_angle.is_finite() {
        return Err(invalid());
    }
    let transform = Transform {
        from: instance.position.as_ref().ok_or_else(invalid)?.clone(),
        to: base_types::Vector2 {
            x_nm: x,
            y_nm: y,
            ..Default::default()
        },
        degrees: (angle_mdeg as f64 / 1000.0 - original_angle).rem_euclid(360.0),
    };
    for field in [
        &mut instance.reference_field,
        &mut instance.value_field,
        &mut instance.datasheet_field,
        &mut instance.description_field,
    ] {
        if let Some(field) = field.as_mut() {
            transform.field(field)?;
        }
    }
    for item in &mut instance.definition.as_mut().ok_or_else(invalid)?.items {
        match item.type_url.as_str() {
            PAD_TYPE_URL => edit::<board_types::Pad>(item, |pad| {
                transform.position(&mut pad.position)?;
                transform.angle(&mut pad.pad_stack.as_mut().ok_or_else(invalid)?.angle)
            })?,
            "type.googleapis.com/kiapi.board.types.Field" => {
                edit::<board_types::Field>(item, |field| transform.field(field))?
            }
            "type.googleapis.com/kiapi.board.types.BoardText" => {
                edit::<board_types::BoardText>(item, |text| transform.text(text))?
            }
            "type.googleapis.com/kiapi.board.types.BoardGraphicShape" => {
                edit::<board_types::BoardGraphicShape>(item, |shape| {
                    if is_copper(shape.layer)? {
                        return Err(invalid());
                    }
                    transform.shape(shape.shape.as_mut().ok_or_else(invalid)?)
                })?
            }
            "type.googleapis.com/kiapi.board.types.Footprint3DModel" => {}
            _ => return Err(invalid()),
        }
    }
    instance.position = MessageField::some(transform.to);
    instance.orientation.mut_or_insert_default().value_degrees = angle_mdeg as f64 / 1000.0;
    Ok(())
}
