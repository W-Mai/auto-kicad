//! Exact current-source disposition policy; no source pin or mutation authority is inferred.
use super::*;

pub(super) fn require_explicit(
    board: &SavedSynchronizationBoard,
    units: &[SynchronizationIneligibleUnit],
    actions: &[SynchronizationAction],
) -> Result<(), String> {
    let mut aliases = BTreeSet::new();
    for unit in units {
        if !aliases.insert(unit.alias.as_str()) {
            return Err("duplicate ineligible source alias".into());
        }
    }
    let mut seen = BTreeSet::new();
    for footprint in board.footprints().values() {
        let Some(path) = footprint
            .path
            .as_deref()
            .map(|path| path.trim_end_matches('/'))
        else {
            continue;
        };
        if !aliases.contains(path) {
            continue;
        }
        if !seen.insert(path) {
            return Err("duplicate_board_ineligible_source_link".into());
        }
        let dispositions = actions
            .iter()
            .filter(|action| match action {
                SynchronizationAction::Keep { footprint_uuid }
                | SynchronizationAction::Detach { footprint_uuid }
                | SynchronizationAction::Remove { footprint_uuid } => {
                    footprint_uuid == &footprint.uuid
                }
                _ => false,
            })
            .count();
        if dispositions != 1 {
            return Err(format!(
                "explicit_ineligible_source_disposition_required: {}: {path}",
                footprint.uuid
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const FP: &str = "11111111-1111-4111-8111-111111111111";
    const SOURCE: &str = "22222222-2222-4222-8222-222222222222";
    fn fixture(path: &str) -> SavedSynchronizationBoard {
        SavedSynchronizationBoard::parse(&format!(r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{FP}") (locked yes) (attr board_only) (property "Reference" "manual") (property "Value" "retained") (path "{path}")))"#)).unwrap()
    }
    #[test]
    fn every_current_ineligible_link_requires_one_exact_disposition_without_reference_fallback() {
        let alias = format!("/{FP}/{SOURCE}");
        let board = fixture(&alias);
        let unit = SynchronizationIneligibleUnit {
            file: "root.kicad_sch".into(),
            sheet_path: format!("/{FP}"),
            source_uuid: SOURCE.into(),
            reference: "#VIRTUAL".into(),
            unit: 1,
            alias: alias.clone(),
            native_exported: false,
            on_board: false,
        };
        assert!(require_explicit(&board, std::slice::from_ref(&unit), &[]).is_err());
        let keep = SynchronizationAction::Keep {
            footprint_uuid: FP.into(),
        };
        require_explicit(
            &board,
            std::slice::from_ref(&unit),
            std::slice::from_ref(&keep),
        )
        .unwrap();
        assert!(
            require_explicit(&board, std::slice::from_ref(&unit), &[keep.clone(), keep]).is_err()
        );
        for action in [
            SynchronizationAction::Detach {
                footprint_uuid: FP.into(),
            },
            SynchronizationAction::Remove {
                footprint_uuid: FP.into(),
            },
        ] {
            require_explicit(&board, std::slice::from_ref(&unit), &[action]).unwrap();
        }
        require_explicit(&fixture("/foreign/root"), &[unit], &[]).unwrap();
    }
}
