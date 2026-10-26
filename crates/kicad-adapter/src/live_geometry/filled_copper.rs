//! Exact filled-copper fingerprints with bounded cyclic normalization.
use super::*;
use serde::Serialize;

type Point = (i64, i64);
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
enum FillNode {
    Point(Point),
    Arc(Point, Point, Point),
}
impl FillNode {
    fn reversed(&self) -> Self {
        match *self {
            Self::Point(p) => Self::Point(p),
            Self::Arc(a, m, b) => Self::Arc(b, m, a),
        }
    }
}

// Linear-time least rotation, including repeated vertices; no coordinates or multiplicities disappear.
fn least_rotation<T: Ord + Clone>(nodes: &[T]) -> Vec<T> {
    let n = nodes.len();
    let (mut i, mut j, mut k) = (0, 1, 0);
    while i < n && j < n && k < n {
        match nodes[(i + k) % n].cmp(&nodes[(j + k) % n]) {
            std::cmp::Ordering::Equal => k += 1,
            std::cmp::Ordering::Greater => {
                i += k + 1;
                if i <= j {
                    i = j + 1;
                }
                k = 0;
            }
            std::cmp::Ordering::Less => {
                j += k + 1;
                if j <= i {
                    j = i + 1;
                }
                k = 0;
            }
        }
    }
    let start = i.min(j);
    (0..n).map(|k| nodes[(start + k) % n].clone()).collect()
}
fn canonical(nodes: Vec<FillNode>) -> Vec<FillNode> {
    let reversed = nodes
        .iter()
        .rev()
        .map(FillNode::reversed)
        .collect::<Vec<_>>();
    least_rotation(&nodes).min(least_rotation(&reversed))
}
fn nm(text: &str) -> Result<i64, String> {
    let (negative, number) = text.strip_prefix('-').map_or((false, text), |s| (true, s));
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 6
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("filled copper requires exact decimal nanometres".into());
    }
    let whole = whole
        .parse::<i64>()
        .map_err(|_| "filled coordinate overflow")?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i64>()
            .map_err(|_| "filled fraction overflow")?
            * 10i64.pow(6 - fraction.len() as u32)
    };
    let value = whole
        .checked_mul(1_000_000)
        .and_then(|v| v.checked_add(fraction))
        .ok_or("filled coordinate overflow")?;
    if value > 2_000_000_000 {
        return Err("filled coordinate exceeds bound".into());
    }
    Ok(if negative { -value } else { value })
}
fn point(node: &Node) -> Result<Point, String> {
    let fields = items(node)?;
    if fields.len() != 3 {
        return Err("invalid filled point".into());
    }
    Ok((nm(atom(&fields[1])?)?, nm(atom(&fields[2])?)?))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct FilledPolygon {
    layer: String,
    island: bool,
    nodes: Vec<FillNode>,
}
fn polygon(node: &Node, total: &mut usize) -> Result<FilledPolygon, String> {
    let (mut layer, mut island, mut nodes) = (None, false, None);
    let mut counted = 0;
    filled_polygon(node, &mut counted)?;
    *total = total
        .checked_add(counted)
        .ok_or("filled point count overflow")?;
    if *total > 500_000 || counted < 3 {
        return Err("filled point count outside bounds".into());
    }
    for field in &items(node)?[1..] {
        match tag(field)? {
            "layer" => {
                let name = scalar(field)?;
                name.parse::<kicad_model::CopperLayer>()
                    .map_err(|_| "filled polygon layer is not canonical copper")?;
                layer = Some(name.to_owned());
            }
            "island" => island = scalar(field)? == "yes",
            "pts" => {
                let values = items(field)?[1..]
                    .iter()
                    .map(|node| match tag(node)? {
                        "xy" => Ok(FillNode::Point(point(node)?)),
                        "arc" => {
                            let f = items(node)?;
                            Ok(FillNode::Arc(point(&f[1])?, point(&f[2])?, point(&f[3])?))
                        }
                        _ => Err("unsupported filled polygon node".into()),
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                nodes = Some(canonical(values));
            }
            _ => return Err("unclassified filled polygon field".into()),
        }
    }
    Ok(FilledPolygon {
        layer: layer.ok_or("missing fill layer")?,
        island,
        nodes: nodes.ok_or("missing filled nodes")?,
    })
}

/// Complete classified direct-zone copper. Equality is separate from definition preservation, DRC and connectivity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FilledCopperEvidence {
    digest: String,
    zone_count: usize,
    polygon_count: usize,
    point_count: usize,
}
impl FilledCopperEvidence {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn point_count(&self) -> usize {
        self.point_count
    }
    pub fn polygon_count(&self) -> usize {
        self.polygon_count
    }
}
impl LiveBoardText {
    /// Preserve zone identities, fill flags, layers, island markers and every point/arc. Only polygon ordering, cyclic start and exact reversed winding normalize.
    pub fn filled_copper(&self) -> Result<FilledCopperEvidence, String> {
        // Also reject ambiguous fill flags and any unclassified derived grammar.
        self.refill_definition_digest()?;
        let mut hash = Sha256::new();
        hash.update(b"auto-kicad-filled-copper-v1");
        let (mut total, mut polygon_count) = (0usize, 0usize);
        for (id, evidence) in &self.zones {
            let zone = items(self.cst.nodes.first().ok_or("missing board root")?)?
                .iter()
                .find(|node| matches!(node,Node::List {span,..} if *span==evidence.span))
                .ok_or("missing native zone node")?;
            let (mut filled, mut polygons) = (false, Vec::new());
            for field in &items(zone)?[1..] {
                match tag(field)? {
                    "fill" => {
                        filled = items(field)?
                            .get(1)
                            .is_some_and(|n| matches!(n, Node::Atom { .. }))
                    }
                    "filled_polygon" => polygons.push(polygon(field, &mut total)?),
                    _ => {}
                }
            }
            if !filled && !polygons.is_empty() {
                return Err("unfilled zone carries filled polygon data".into());
            }
            polygons.sort();
            polygon_count += polygons.len();
            let bytes = serde_json::to_vec(&(id, filled, polygons)).map_err(|e| e.to_string())?;
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        Ok(FilledCopperEvidence {
            digest: hex::encode(hash.finalize()),
            zone_count: self.zones.len(),
            polygon_count,
            point_count: total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn board(polygon: &str) -> LiveBoardText {
        LiveBoardText::parse(&format!(
            "(kicad_pcb (version 20260206) (zone (uuid z) (fill yes) {polygon}))"
        ))
        .unwrap()
    }
    #[test]
    fn rotation_is_linear_and_preserves_repetitions() {
        for n in 1..64 {
            let data = (0..n).map(|i| (i * 7 + 3) % 5).collect::<Vec<_>>();
            let oracle = (0..n)
                .map(|i| {
                    data[i..]
                        .iter()
                        .chain(&data[..i])
                        .copied()
                        .collect::<Vec<_>>()
                })
                .min()
                .unwrap();
            assert_eq!(least_rotation(&data), oracle);
        }
        assert_eq!(least_rotation(&vec![0; 500_000]).len(), 500_000);
    }
    #[test]
    fn fingerprints_normalize_only_exact_geometry_order() {
        let a =
            "(filled_polygon (layer F.Cu) (island yes) (pts (xy 0 0) (xy 2 0) (xy 2 2) (xy 0 0)))";
        let b =
            "(filled_polygon (layer F.Cu) (island yes) (pts (xy 2 2) (xy 2 0) (xy 0 0) (xy 0 0)))";
        assert_eq!(
            board(a).filled_copper().unwrap(),
            board(b).filled_copper().unwrap()
        );
        for changed in [
            a.replace("F.Cu", "In1.Cu"),
            a.replace("(island yes)", ""),
            a.replace("(xy 2 0)", "(xy 2.000001 0)"),
            a.replacen("(xy 0 0)", "", 1),
            format!("{a}{a}"),
        ] {
            assert_ne!(
                board(a).filled_copper().unwrap(),
                board(&changed).filled_copper().unwrap()
            );
        }
        let arc =
            "(filled_polygon (layer F.Cu) (pts (arc (start 0 0) (mid 1 1) (end 2 0)) (xy 1 -1)))";
        let reversed =
            "(filled_polygon (layer F.Cu) (pts (xy 1 -1) (arc (start 2 0) (mid 1 1) (end 0 0))))";
        assert_eq!(
            board(arc).filled_copper().unwrap(),
            board(reversed).filled_copper().unwrap()
        );
        assert_ne!(
            board(arc).filled_copper().unwrap(),
            board(&arc.replace("mid 1 1", "mid 1 1.000001"))
                .filled_copper()
                .unwrap()
        );
        assert_eq!(
            board(&format!("{a}{arc}")).filled_copper().unwrap(),
            board(&format!("{arc}{b}")).filled_copper().unwrap()
        );
        for invalid in [
            a.replace("0 0", "0.0000001 0"),
            a.replace("0 0", "NaN 0"),
            a.replace("0 0", "2001 0"),
            a.replace("(island yes)", "(island yes) (unknown 1)"),
        ] {
            assert!(board(&invalid).filled_copper().is_err());
        }
    }
}
