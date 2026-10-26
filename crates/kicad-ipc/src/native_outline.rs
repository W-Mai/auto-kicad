//! Typed native Edge.Cuts primitives and exact creation receipts.
use crate::*;
use std::collections::BTreeSet;

const SHAPE: &str = "type.googleapis.com/kiapi.board.types.BoardGraphicShape";
const MAX_COORD: i64 = 2_000_000_000;
const MAX_SPAN: i64 = 1_000_000_000;
const MAX_PRIMITIVES: usize = 512;
type Point = (i64, i64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutlineGeometry {
    Segment {
        start: Point,
        end: Point,
    },
    Arc {
        start: Point,
        mid: Point,
        end: Point,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlinePrimitive {
    pub geometry: OutlineGeometry,
    pub width_nm: i64,
}
fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidBoardGraphic(reason)
}
fn vector((x_nm, y_nm): Point) -> MessageField<base_types::Vector2> {
    MessageField::some(base_types::Vector2 {
        x_nm,
        y_nm,
        ..Default::default()
    })
}
impl OutlinePrimitive {
    fn points(&self) -> Vec<Point> {
        match self.geometry {
            OutlineGeometry::Segment { start, end } => vec![start, end],
            OutlineGeometry::Arc { start, mid, end } => vec![start, mid, end],
        }
    }
    /// Validate primitive coordinates; closed-loop topology and copper clearance belong to planning.
    pub fn validate(&self) -> Result<(), IpcError> {
        if !(1..=MAX_COORD).contains(&self.width_nm) {
            return Err(invalid("outline width exceeds native bounds"));
        }
        let points = self.points();
        if points.iter().any(|&(x, y)| {
            !(-MAX_COORD..=MAX_COORD).contains(&x) || !(-MAX_COORD..=MAX_COORD).contains(&y)
        }) {
            return Err(invalid("outline coordinates exceed native bounds"));
        }
        if points.iter().collect::<BTreeSet<_>>().len() != points.len() {
            return Err(invalid("outline control points must differ"));
        }
        if let OutlineGeometry::Arc {
            start: (ax, ay),
            mid: (bx, by),
            end: (cx, cy),
        } = self.geometry
        {
            let cross = (i128::from(bx) - i128::from(ax)) * (i128::from(cy) - i128::from(ay))
                - (i128::from(by) - i128::from(ay)) * (i128::from(cx) - i128::from(ax));
            if cross == 0 {
                return Err(invalid("outline arc points must not be collinear"));
            }
        }
        Ok(())
    }
    fn native(&self) -> Result<board_types::BoardGraphicShape, IpcError> {
        self.validate()?;
        let geometry = match self.geometry {
            OutlineGeometry::Segment { start, end } => {
                base_types::graphic_shape::Geometry::Segment(base_types::GraphicSegmentAttributes {
                    start: vector(start),
                    end: vector(end),
                    ..Default::default()
                })
            }
            OutlineGeometry::Arc { start, mid, end } => {
                base_types::graphic_shape::Geometry::Arc(base_types::GraphicArcAttributes {
                    start: vector(start),
                    mid: vector(mid),
                    end: vector(end),
                    ..Default::default()
                })
            }
        };
        Ok(board_types::BoardGraphicShape {
            layer: board_types::BoardLayer::BL_Edge_Cuts.into(),
            locked: base_types::LockedState::LS_UNLOCKED.into(),
            shape: MessageField::some(base_types::GraphicShape {
                geometry: Some(geometry),
                attributes: MessageField::some(base_types::GraphicAttributes {
                    stroke: MessageField::some(base_types::StrokeAttributes {
                        width: MessageField::some(base_types::Distance {
                            value_nm: self.width_nm,
                            ..Default::default()
                        }),
                        style: enums::StrokeLineStyle::SLS_SOLID.into(),
                        ..Default::default()
                    }),
                    fill: MessageField::some(base_types::GraphicFillAttributes {
                        fill_type: base_types::GraphicFillType::GFT_UNFILLED.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
    }
}

/// Complete accepted native payload, including fields outside the requested geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatedOutlineReceipt {
    pub request_index: usize,
    pub source_uuid: String,
    native: board_types::BoardGraphicShape,
}
impl CreatedOutlineReceipt {
    pub fn native(&self) -> &board_types::BoardGraphicShape {
        &self.native
    }
    /// Only the transient board parent is omitted; every persistent native field must agree.
    pub fn matches_saved(&self, actual: &board_types::BoardGraphicShape) -> bool {
        let mut before = self.native.clone();
        let mut after = actual.clone();
        before.parent = MessageField::none();
        after.parent = MessageField::none();
        before == after
    }
}
fn validate_requests(primitives: &[OutlinePrimitive]) -> Result<(), IpcError> {
    if primitives.is_empty() || primitives.len() > MAX_PRIMITIVES {
        return Err(invalid("outline batch must contain 1..512 primitives"));
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for primitive in primitives {
        primitive.validate()?;
        for (x, y) in primitive.points() {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if max_x - min_x > MAX_SPAN || max_y - min_y > MAX_SPAN {
        return Err(invalid("outline control-point span exceeds bound"));
    }
    Ok(())
}
fn receipts(
    reply: &editor_commands::CreateItemsResponse,
    requested: &[OutlinePrimitive],
) -> Result<Vec<CreatedOutlineReceipt>, IpcError> {
    if reply.created_items.len() != requested.len() {
        return Err(IpcError::CreateItemsCountMismatch {
            expected: requested.len(),
            actual: reply.created_items.len(),
        });
    }
    let mut ids = BTreeSet::new();
    reply
        .created_items
        .iter()
        .zip(requested)
        .enumerate()
        .map(|(index, (result, request))| {
            if result.status.as_ref().map(|s| s.code.enum_value())
                != Some(Ok(editor_commands::ItemStatusCode::ISC_OK))
            {
                return Err(invalid("outline creation item failed"));
            }
            let item = result
                .item
                .as_ref()
                .ok_or(IpcError::MissingResponseMessage)?;
            if item.type_url != SHAPE {
                return Err(invalid("outline receipt type differs"));
            }
            let native = board_types::BoardGraphicShape::parse_from_bytes(&item.value)
                .map_err(|_| IpcError::MalformedEnvelope)?;
            let id = kiid_value(native.id.as_ref())
                .ok_or(invalid("outline receipt identity missing"))?;
            if id.is_empty() || id.len() > 128 || id.contains('\0') || !ids.insert(id.clone()) {
                return Err(invalid(
                    "outline receipt identities must be unique and bounded",
                ));
            }
            let expected = request.native()?;
            let shape = native
                .shape
                .as_ref()
                .ok_or(invalid("outline receipt shape missing"))?;
            let attrs = shape
                .attributes
                .as_ref()
                .ok_or(invalid("outline receipt attributes missing"))?;
            let stroke = attrs
                .stroke
                .as_ref()
                .ok_or(invalid("outline receipt stroke missing"))?;
            if native.layer != expected.layer
                || native.locked != expected.locked
                || shape.geometry != expected.shape.as_ref().unwrap().geometry
                || stroke.width.as_ref().map(|w| w.value_nm) != Some(request.width_nm)
                || stroke.style.enum_value() != Ok(enums::StrokeLineStyle::SLS_SOLID)
                || attrs.fill.as_ref().map(|f| f.fill_type.enum_value())
                    != Some(Ok(base_types::GraphicFillType::GFT_UNFILLED))
                || native.net.as_ref().is_some_and(|n| {
                    !n.name.is_empty() || n.code.as_ref().is_some_and(|code| code.value != 0)
                })
            {
                return Err(invalid(
                    "native outline receipt differs from requested geometry or attributes",
                ));
            }
            Ok(CreatedOutlineReceipt {
                request_index: index,
                source_uuid: id,
                native,
            })
        })
        .collect()
}

pub(super) fn removable_outline(shape: &board_types::BoardGraphicShape) -> Result<(), IpcError> {
    if shape.layer.enum_value() != Ok(board_types::BoardLayer::BL_Edge_Cuts)
        || shape.locked.enum_value() != Ok(base_types::LockedState::LS_UNLOCKED)
    {
        return Err(invalid(
            "outline removal requires an unlocked Edge.Cuts shape",
        ));
    }
    if !matches!(
        shape.shape.as_ref().and_then(|s| s.geometry.as_ref()),
        Some(
            base_types::graphic_shape::Geometry::Segment(_)
                | base_types::graphic_shape::Geometry::Arc(_)
                | base_types::graphic_shape::Geometry::Circle(_)
                | base_types::graphic_shape::Geometry::Rectangle(_)
                | base_types::graphic_shape::Geometry::Polygon(_)
        )
    ) {
        return Err(invalid("unsupported outline removal geometry"));
    }
    Ok(())
}
impl<T: KiCadTransport> KiCadClient<T> {
    /// Create native primitives inside the caller's transaction. This does not validate board topology.
    pub fn create_outline_primitives(
        &mut self,
        filename: &str,
        primitives: &[OutlinePrimitive],
    ) -> Result<Vec<CreatedOutlineReceipt>, IpcError> {
        validate_requests(primitives)?;
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let document = self.require_current_board(&token, filename)?;
        let items = primitives
            .iter()
            .map(|p| {
                Ok(Any {
                    type_url: SHAPE.into(),
                    value: p
                        .native()?
                        .write_to_bytes()
                        .map_err(|_| IpcError::MalformedEnvelope)?,
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>, IpcError>>()?;
        let reply: editor_commands::CreateItemsResponse = self.bound_item_command(
            CREATE_ITEMS_TYPE_URL,
            CREATE_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::CreateItems {
                header: MessageField::some(board_item_header(filename)),
                items,
                ..Default::default()
            },
        )?;
        self.validate_item_reply(reply.header.clone(), reply.status, filename)?;
        let receipts = receipts(&reply, primitives)?;
        if self.require_current_board(&token, filename)? != document {
            return Err(invalid("document changed during outline creation"));
        }
        Ok(receipts)
    }
    /// Remove exact observed board-level Edge.Cuts IDs, with fresh evidence before deletion.
    pub fn delete_outline_items(
        &mut self,
        filename: &str,
        before: &NativeBoardGeometry,
        ids: &[String],
    ) -> Result<DeleteItemsReceipt, IpcError> {
        super::existing_items::identifiers(ids, 100)?;
        for id in ids {
            removable_outline(
                before
                    .shapes()
                    .get(id)
                    .ok_or(invalid("outline removal target missing"))?,
            )?;
        }
        if self.capture_board_geometry(filename)? != *before {
            return Err(invalid("outline removal evidence changed"));
        }
        self.delete_board_items(filename, ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment() -> OutlinePrimitive {
        OutlinePrimitive {
            geometry: OutlineGeometry::Segment {
                start: (0, 0),
                end: (1_000_000, 0),
            },
            width_nm: 50_000,
        }
    }
    #[test]
    fn outline_bounds_reject_degenerate_or_oversized_native_inputs() {
        assert!(validate_requests(&vec![segment(); 512]).is_ok());
        assert!(validate_requests(&vec![segment(); 513]).is_err());
        assert!(validate_requests(&[]).is_err());
        for geometry in [
            OutlineGeometry::Segment {
                start: (0, 0),
                end: (0, 0),
            },
            OutlineGeometry::Arc {
                start: (0, 0),
                mid: (1, 1),
                end: (2, 2),
            },
            OutlineGeometry::Arc {
                start: (0, 0),
                mid: (1, 1),
                end: (i64::MAX, 0),
            },
        ] {
            assert!(
                OutlinePrimitive {
                    geometry,
                    width_nm: 1
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            validate_requests(&[OutlinePrimitive {
                geometry: OutlineGeometry::Segment {
                    start: (-MAX_COORD, 0),
                    end: (MAX_COORD, 0)
                },
                width_nm: 1
            }])
            .is_err()
        );
    }
    #[test]
    fn outline_receipts_bind_geometry_and_preserve_all_native_fields() {
        let requested = segment();
        let mut native = requested.native().unwrap();
        native.id = MessageField::some(base_types::KIID {
            value: "created".into(),
            ..Default::default()
        });
        native
            .special_fields
            .mut_unknown_fields()
            .add_varint(1000, 42);
        let item = editor_commands::ItemCreationResult {
            item: MessageField::some(Any {
                type_url: SHAPE.into(),
                value: native.write_to_bytes().unwrap(),
                ..Default::default()
            }),
            status: MessageField::some(editor_commands::ItemStatus {
                code: editor_commands::ItemStatusCode::ISC_OK.into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let reply = editor_commands::CreateItemsResponse {
            created_items: vec![item],
            ..Default::default()
        };
        let accepted = receipts(&reply, std::slice::from_ref(&requested)).unwrap();
        assert!(accepted[0].matches_saved(&native));
        let mut changed = native.clone();
        changed
            .special_fields
            .mut_unknown_fields()
            .add_varint(1001, 1);
        assert!(!accepted[0].matches_saved(&changed));
        changed = native;
        changed.locked = base_types::LockedState::LS_LOCKED.into();
        assert!(removable_outline(&changed).is_err());
        let mut different = requested;
        different.width_nm += 1;
        assert!(receipts(&reply, &[different]).is_err());
        let mut duplicates = reply.clone();
        duplicates.created_items.extend(reply.created_items);
        assert!(receipts(&duplicates, &[segment(), segment()]).is_err());
    }
}
