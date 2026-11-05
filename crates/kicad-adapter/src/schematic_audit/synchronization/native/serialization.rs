//! Complete native saved content across transient generated-zone serialization order.
use super::*;

pub(super) fn generated_saved_signature(text: &str) -> Result<Option<String>, String> {
    let partition = crate::live_geometry::TeardropTextPartition::parse(text)?;
    if partition.generated_count() == 0 {
        return Ok(None);
    }
    let complete = serde_json::json!({"authored":partition.authored().closed_refill_definitions()?,
        "copper":partition.authored().filled_copper()?,"generated":partition.generated_content()?});
    Ok(Some(hash(
        &serde_json::to_vec(&complete).map_err(|e| e.to_string())?,
    )))
}

impl NativeBoardProjection {
    pub(in crate::schematic_audit::synchronization) fn generated_saved_signature(
        &self,
    ) -> Result<Option<String>, String> {
        generated_saved_signature(self.board.text())
    }
    pub(in crate::schematic_audit::synchronization) fn same_saved_content(
        &self,
        other: &Self,
        regions: bool,
    ) -> Result<bool, String> {
        if regions {
            let first = self.generated_saved_signature()?;
            let second = other.generated_saved_signature()?;
            if first.is_some() || second.is_some() {
                return Ok(first == second);
            }
        }
        Ok(self.board.text() == other.board.text())
    }
    pub(in crate::schematic_audit::synchronization) fn same_report(
        &self,
        other: &Self,
        regions: bool,
    ) -> Result<bool, String> {
        if !regions || self.generated_saved_signature()?.is_none() {
            return Ok(self.report == other.report);
        }
        if !self.same_saved_content(other, true)? {
            return Ok(false);
        }
        self.matches_original_report(&other.report, other.generated_saved_signature()?.as_deref())
    }
    pub(in crate::schematic_audit::synchronization) fn matches_original_report(
        &self,
        original: &NativeReport,
        generated_signature: Option<&str>,
    ) -> Result<bool, String> {
        let Some(signature) = generated_signature else {
            return Ok(self.report == *original);
        };
        if self.generated_saved_signature()?.as_deref() != Some(signature) {
            return Err(
                "complete generated native content differs from the recorded signature".into(),
            );
        }
        let mut expected = original.clone();
        // Both raw save hashes remain bound to captured bytes; every other native field is exact.
        expected.native_saved_sha256 = self.report.native_saved_sha256.clone();
        Ok(self.report == expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_generated_signature_accepts_only_permutation_of_complete_derived_forms() {
        let a = r#"(zone (layer F.Mask) (attr (teardrop (type padvia))) (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1))) (future_zone 42))"#;
        let b = r#"(zone (layer B.Mask) (attr (teardrop (type padvia))) (polygon (pts (xy 2 0) (xy 3 0) (xy 2 1))) (future_zone 99))"#;
        let board = |zones: &str| {
            format!("(kicad_pcb (version 20260206) (future_board \"exact\") {zones})")
        };
        let first = board(&format!("{a} {b}"));
        let second = board(&format!("{b} {a}"));
        let signature = generated_saved_signature(&first).unwrap().unwrap();
        assert_eq!(
            Some(signature.clone()),
            generated_saved_signature(&second).unwrap()
        );
        for changed in [
            second.replace("future_zone 42", "future_zone 43"),
            second.replace("future_board \"exact\"", "future_board \"changed\""),
            board(b),
        ] {
            assert_ne!(
                Some(signature.clone()),
                generated_saved_signature(&changed).unwrap()
            );
        }
        assert_eq!(None, generated_saved_signature(&board("")).unwrap());
    }
}
