//! Saved/live binding for the exact footprint serializer header emitted by KiCad 10.0.7.
use super::*;

impl LiveBoardText {
    /// Bind saved authored definitions and copper without weakening live mutation comparisons.
    pub fn verify_saved_geometry(&self, saved: &Self) -> Result<(), String> {
        let live = self.without_footprint_serializer_headers()?;
        let saved = saved.without_footprint_serializer_headers()?;
        if live.refill_definition_digest()? != saved.refill_definition_digest()?
            || live.filled_copper()? != saved.filled_copper()?
        {
            return Err(
                "saved authored geometry differs from the complete live observation".into(),
            );
        }
        Ok(())
    }

    fn without_footprint_serializer_headers(&self) -> Result<Self, String> {
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        let mut spans = Vec::new();
        for node in &items(root)?[1..] {
            if tag(node)? != "footprint" {
                continue;
            }
            let mut headers = BTreeMap::new();
            for field in &items(node)?[1..] {
                if !matches!(field, Node::List { .. }) {
                    continue;
                }
                let name = tag(field)?;
                let expected = match name {
                    "version" => "20260206",
                    "generator" => "pcbnew",
                    "generator_version" => "10.0",
                    _ => continue,
                };
                if scalar(field)? != expected || headers.insert(name, field).is_some() {
                    return Err("unclassified or duplicate footprint serializer header".into());
                }
            }
            if !headers.is_empty() && headers.len() != 3 {
                return Err("incomplete footprint serializer header".into());
            }
            for field in headers.values() {
                let Node::List { span, .. } = field else {
                    unreachable!()
                };
                spans.push(*span);
            }
        }
        spans.sort_by_key(|s| s.start);
        let mut text = self.raw_text().to_owned();
        for span in spans.into_iter().rev() {
            text.replace_range(span.start..span.end, "");
        }
        Self::parse(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_binding_only_projects_complete_direct_native_headers() {
        let saved = r#"(kicad_pcb (version 20260206) (setup (zone_defaults)) (footprint "x" (uuid fp) (at 1 2) (future 7) (property "Value" "pcbnew")))"#;
        let header = r#"(version 20260206) (generator "pcbnew") (generator_version "10.0")"#;
        let live = saved
            .replace("(zone_defaults)", "")
            .replace("(uuid fp)", &format!("{header} (uuid fp)"));
        let parse = |s: &str| LiveBoardText::parse(s).unwrap();
        parse(&live).verify_saved_geometry(&parse(saved)).unwrap();
        assert_ne!(
            parse(&live).refill_definition_digest().unwrap(),
            parse(saved).refill_definition_digest().unwrap()
        );
        for (from, to) in [
            ("at 1 2", "at 1 3"),
            ("future 7", "future 8"),
            ("uuid fp", "uuid other"),
            ("\"Value\" \"pcbnew\"", "\"Value\" \"other\""),
        ] {
            assert!(
                parse(&live)
                    .verify_saved_geometry(&parse(&saved.replace(from, to)))
                    .is_err()
            );
        }
        for bad in [
            "(version 20260206)",
            "(version 1) (generator pcbnew) (generator_version 10.0)",
            "(version 20260206) (version 20260206) (generator pcbnew) (generator_version 10.0)",
        ] {
            assert!(
                parse(&live.replace(header, bad))
                    .verify_saved_geometry(&parse(saved))
                    .is_err()
            );
        }
        let nested = saved.replace("(future 7)", &format!("(future 7 {header})"));
        assert!(parse(&live).verify_saved_geometry(&parse(&nested)).is_err());
    }
}
