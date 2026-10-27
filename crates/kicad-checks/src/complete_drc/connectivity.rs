//! Exact report comparison with independently proven physical endpoint correspondence.
use super::{CompleteDrcReport, Node, ParseError};
use crate::connectivity::ClosedConnectivityEvidence;
use std::collections::{BTreeMap, BTreeSet};

fn invalid(message: &str) -> ParseError {
    ParseError::Schema(message.into())
}
fn mapped<'a>(id: &'a str, mapping: &'a BTreeMap<String, String>) -> &'a str {
    mapping.get(id).map(String::as_str).unwrap_or(id)
}

fn verify_components(
    left: &ClosedConnectivityEvidence,
    right: &ClosedConnectivityEvidence,
    mapping: &BTreeMap<String, String>,
) -> Result<(), ParseError> {
    let mut seen = BTreeSet::new();
    for endpoint in left.endpoints() {
        let id = mapped(endpoint.uuid(), mapping);
        let other = right
            .endpoint(id)
            .ok_or_else(|| invalid("mapped native endpoint missing"))?;
        if !seen.insert(id)
            || endpoint.kind() != other.kind()
            || endpoint.net() != other.net()
            || endpoint.position_nm() != other.position_nm()
        {
            return Err(invalid(
                "mapped native endpoint identity, net, kind or position differs",
            ));
        }
        let mut members: Vec<_> = left
            .component(endpoint.uuid())
            .ok_or_else(|| invalid("missing native component"))?
            .iter()
            .map(|member| mapped(member, mapping))
            .collect();
        members.sort();
        if members
            != right
                .component(id)
                .ok_or_else(|| invalid("missing mapped component"))?
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        {
            return Err(invalid("complete physical component membership differs"));
        }
    }
    if seen.len() != right.endpoints().len() {
        return Err(invalid("native endpoint set differs"));
    }
    Ok(())
}

// Convert the original JSON token exactly; binary floating point and rounding are forbidden.
fn nanometres(node: &Node) -> Result<i64, ParseError> {
    let Node::Scalar(text) = node else {
        return Err(invalid("position must be numeric"));
    };
    if text.len() > 64 {
        return Err(invalid("position token exceeds bound"));
    }
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let exponent: i32 = exponent
        .parse()
        .map_err(|_| invalid("invalid position exponent"))?;
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |s| (true, s));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("invalid position number"));
    }
    let mut value: i128 = digits
        .parse()
        .map_err(|_| invalid("position number exceeds bound"))?;
    let power = exponent
        .checked_add(6)
        .and_then(|n| n.checked_sub(fraction.len() as i32))
        .filter(|n| n.unsigned_abs() <= 38)
        .ok_or_else(|| invalid("position precision exceeds bound"))?;
    let scale = 10i128
        .checked_pow(power.unsigned_abs())
        .ok_or_else(|| invalid("position scale exceeds bound"))?;
    if power >= 0 {
        value = value
            .checked_mul(scale)
            .ok_or_else(|| invalid("position exceeds bound"))?;
    } else {
        if value % scale != 0 {
            return Err(invalid("position is not exact integral nanometres"));
        }
        value /= scale;
    }
    if negative {
        value = -value;
    }
    let value = i64::try_from(value).map_err(|_| invalid("position exceeds bound"))?;
    if value.unsigned_abs() > 2_000_000_000 {
        return Err(invalid("position exceeds board bounds"));
    }
    Ok(value)
}

fn string<'a>(object: &'a BTreeMap<String, Node>, key: &str) -> Result<&'a str, ParseError> {
    match object.get(key) {
        Some(Node::String(value)) => Ok(value),
        _ => Err(invalid("missing required string field")),
    }
}

#[derive(Clone, Debug)]
struct Endpoint {
    id: String,
    component: String,
    net: String,
    display: Node,
    unknown: Node,
}
impl Endpoint {
    fn parse(
        node: &Node,
        proof: &ClosedConnectivityEvidence,
        target: &ClosedConnectivityEvidence,
        mapping: &BTreeMap<String, String>,
    ) -> Result<Self, ParseError> {
        let Node::Object(fields) = node else {
            return Err(invalid("unconnected endpoint is not an object"));
        };
        let uuid = string(fields, "uuid")?;
        string(fields, "description")?;
        let native = proof
            .endpoint(uuid)
            .ok_or_else(|| invalid("unconnected endpoint is unobserved"))?;
        if native.net().is_empty() {
            return Err(invalid("netless unconnected endpoint unsupported"));
        }
        let Some(Node::Object(position)) = fields.get("pos") else {
            return Err(invalid("missing endpoint position"));
        };
        let coordinate = |name| {
            nanometres(
                position
                    .get(name)
                    .ok_or_else(|| invalid("missing position coordinate"))?,
            )
        };
        if [coordinate("x")?, coordinate("y")?] != native.position_nm() {
            return Err(invalid(
                "diagnostic position differs from exact native reporting position",
            ));
        }
        let id = mapped(uuid, mapping).to_owned();
        let component = target
            .component(&id)
            .and_then(|members| members.first())
            .ok_or_else(|| invalid("unconnected endpoint has no mapped native component"))?
            .clone();
        let mut exact = fields.clone();
        exact.insert("uuid".into(), Node::String(id.clone()));
        let display = Node::Object(exact);
        let mut unknown = fields.clone();
        unknown.remove("uuid");
        unknown.remove("description");
        let mut position = position.clone();
        position.remove("x");
        position.remove("y");
        unknown.insert("pos".into(), Node::Object(position));
        Ok(Self {
            id,
            component,
            net: native.net().into(),
            display,
            unknown: Node::Object(unknown),
        })
    }
}

type EdgeKey = (Node, [(String, Node); 2]);
#[derive(Default)]
struct Diagnostics {
    edges: BTreeMap<EdgeKey, usize>,
    displays: BTreeMap<String, Node>,
}
impl Diagnostics {
    fn insert(&mut self, header: Node, mut endpoints: [Endpoint; 2]) -> Result<(), ParseError> {
        if endpoints[0].component == endpoints[1].component || endpoints[0].net != endpoints[1].net
        {
            return Err(invalid(
                "missing connection must join distinct components of one named net",
            ));
        }
        if endpoints[0].component > endpoints[1].component {
            endpoints.swap(0, 1);
        }
        for endpoint in &endpoints {
            if let Some(previous) = self
                .displays
                .insert(endpoint.id.clone(), endpoint.display.clone())
                && previous != endpoint.display
            {
                return Err(invalid(
                    "one diagnostic UUID has contradictory native endpoint fields",
                ));
            }
        }
        let key = (
            header,
            endpoints.map(|endpoint| (endpoint.component, endpoint.unknown)),
        );
        *self.edges.entry(key).or_default() += 1;
        Ok(())
    }
    fn equivalent_to(&self, other: &Self) -> bool {
        self.edges == other.edges
            && self
                .displays
                .iter()
                .all(|(id, value)| other.displays.get(id).is_none_or(|other| value == other))
    }
}

fn extract(
    tree: &mut Node,
    proof: &ClosedConnectivityEvidence,
    target: &ClosedConnectivityEvidence,
    mapping: &BTreeMap<String, String>,
) -> Result<Diagnostics, ParseError> {
    let Node::Object(root) = tree else {
        return Err(invalid("report is not an object"));
    };
    if string(root, "kicad_version")? != "10.0.7" || string(root, "coordinate_units")? != "mm" {
        return Err(invalid(
            "unsupported connectivity report version or coordinate units",
        ));
    }
    let Some(Node::Array(diagnostics)) = root.remove("unconnected_items") else {
        return Err(invalid("missing complete unconnected diagnostics"));
    };
    if diagnostics.len() > 4096 {
        return Err(invalid("unconnected diagnostic count exceeds bound"));
    }
    let mut groups = Diagnostics::default();
    for diagnostic in diagnostics {
        let Node::Object(mut fields) = diagnostic else {
            return Err(invalid("invalid unconnected diagnostic"));
        };
        if string(&fields, "type")? != "unconnected_items"
            || string(&fields, "description")? != "Missing connection between items"
            || !matches!(string(&fields, "severity")?, "error" | "warning")
        {
            return Err(invalid(
                "unsupported unconnected diagnostic rule, label or severity",
            ));
        }
        let Some(Node::Array(items)) = fields.remove("items") else {
            return Err(invalid("missing unconnected endpoints"));
        };
        if items.len() != 2 {
            return Err(invalid(
                "unconnected diagnostic requires exactly two endpoints",
            ));
        }
        let endpoints = [
            Endpoint::parse(&items[0], proof, target, mapping)?,
            Endpoint::parse(&items[1], proof, target, mapping)?,
        ];
        groups.insert(Node::Object(fields), endpoints)?;
    }
    Ok(groups)
}

impl CompleteDrcReport {
    /// Compare complete reports with closed native physical-component evidence.
    /// The caller must additionally bind both report executions to these exact saved inputs
    /// and establish live agreement. This method alone does not authorize an edit.
    pub fn equivalent_with_closed_connectivity(
        &self,
        other: &Self,
        identities: &BTreeMap<String, String>,
        left_proof: &ClosedConnectivityEvidence,
        right_proof: &ClosedConnectivityEvidence,
    ) -> Result<bool, ParseError> {
        // Reuse the strict map validation, including collisions with original diagnostic identities.
        self.equivalent_to(other, identities)?;
        verify_components(left_proof, right_proof, identities)?;
        let mut left = Node::parse(&self.raw, 0)?;
        let mut right = Node::parse(&other.raw, 0)?;
        let a = extract(&mut left, left_proof, right_proof, identities)?;
        let b = extract(&mut right, right_proof, right_proof, &BTreeMap::new())?;
        left.canonicalize(identities);
        right.canonicalize(&BTreeMap::new());
        Ok(left == right && a.equivalent_to(&b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_positions_require_exact_integral_nanometres() {
        for (token, expected) in [
            ("1.000001", 1_000_001),
            ("1e-6", 1),
            ("-0.000001", -1),
            ("2e3", 2_000_000_000),
            ("0.00", 0),
        ] {
            assert_eq!(nanometres(&Node::Scalar(token.into())).unwrap(), expected);
        }
        for token in ["0.0000001", "1e99", "-2000.000001", "null", "true", "1e-99"] {
            assert!(nanometres(&Node::Scalar(token.into())).is_err());
        }
    }
    #[test]
    fn component_multiset_cannot_hide_changed_display_on_a_shared_identity() {
        let endpoint = |id: &str, label: &str| Endpoint {
            id: id.into(),
            component: "a".into(),
            net: "GND".into(),
            display: Node::String(label.into()),
            unknown: Node::Scalar("1.00".into()),
        };
        let other = Endpoint {
            component: "z".into(),
            ..endpoint("z", "pad")
        };
        let edge = |id, label| [endpoint(id, label), other.clone()];
        let header = Node::String("missing connection".into());
        let mut left = Diagnostics::default();
        left.insert(header.clone(), edge("a", "old")).unwrap();
        left.insert(header.clone(), edge("b", "old")).unwrap();
        let mut right = Diagnostics::default();
        right.insert(header.clone(), edge("a", "old")).unwrap();
        right.insert(header.clone(), edge("b", "changed")).unwrap();
        assert!(!left.equivalent_to(&right));
        let mut alternative = Diagnostics::default();
        alternative
            .insert(header.clone(), edge("c", "new"))
            .unwrap();
        alternative
            .insert(header.clone(), edge("c", "new"))
            .unwrap();
        assert!(left.equivalent_to(&alternative));
        assert!(alternative.equivalent_to(&left));
        assert!(
            alternative
                .insert(header.clone(), edge("c", "contradiction"))
                .is_err()
        );
        let mut missing = Diagnostics::default();
        missing.insert(header.clone(), edge("c", "new")).unwrap();
        assert!(!left.equivalent_to(&missing));
        let mut changed = edge("d", "other");
        changed[0].unknown = Node::Scalar("1.0".into());
        missing.insert(header, changed).unwrap();
        assert!(!left.equivalent_to(&missing));
    }
}
