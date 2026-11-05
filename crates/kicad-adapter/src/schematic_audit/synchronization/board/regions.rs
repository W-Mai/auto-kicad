//! Native comparison projection gated by independently matched complete refill content.
use super::*;

impl SavedSynchronizationBoard {
    /// Hash unchanged native definitions after separately proving all omitted derived content.
    pub(in crate::schematic_audit::synchronization) fn protected_refilled_native_digest(
        &self,
        changes: &[SynchronizationDifference],
        _content: &crate::live_geometry::ClosedRefillCandidate,
    ) -> Result<String, String> {
        // This projection is only for comparison; it is never loaded, saved or published.
        let root = self
            .cst
            .nodes
            .first()
            .ok_or("missing native region board")?;
        let mut patches = Vec::new();
        for zone in children(root, "zone")? {
            let generated = optional(zone, "attr")?
                .map(|attr| optional(attr, "teardrop"))
                .transpose()?
                .flatten()
                .is_some();
            if generated {
                patches.push((span(zone), String::new()));
                continue;
            }
            for polygon in children(zone, "filled_polygon")? {
                patches.push((span(polygon), String::new()));
            }
            if let Some(fill) = optional(zone, "fill")?
                && let Some(marker) = items(fill)?.get(1)
                && atom(marker).is_ok_and(|value| value == "yes")
            {
                patches.push((span(marker), String::new()));
            }
        }
        patches.sort_by_key(|(span, _)| (span.start, span.end));
        if patches
            .windows(2)
            .any(|pair| pair[0].0.end > pair[1].0.start)
        {
            return Err("overlapping native region projection".into());
        }
        let mut projected = self.text().to_owned();
        for (span, text) in patches.into_iter().rev() {
            projected.replace_range(span.start..span.end, &text);
        }
        let projected = SavedSynchronizationBoard::parse(&projected)?;
        projected.protected_native_digest(changes)
    }
}
