//! Target-specific native rule authority for a composed PCB/project candidate.
use super::*;

fn project_selected<T: Eq>(original: &T, without: &T, after: &T) -> bool {
    without != after || original == after
}

fn members(class: &NativeEffectiveClass) -> BTreeSet<&str> {
    class
        .constituents
        .iter()
        .map(|member| member.name.as_str())
        .collect()
}

fn class_transition_allowed(
    original: &NativeEffectiveClass,
    without: &NativeEffectiveClass,
    after: &NativeEffectiveClass,
    assignment_selected: bool,
) -> bool {
    if original == after || assignment_selected || members(without) != members(after) {
        return true;
    }
    project_selected(&original.name, &without.name, &after.name)
        && project_selected(&original.priority, &without.priority, &after.priority)
        && project_selected(
            &original.description,
            &without.description,
            &after.description,
        )
        && project_selected(
            &original.constituents,
            &without.constituents,
            &after.constituents,
        )
        && project_selected(
            &original.tuning_profile.defined,
            &without.tuning_profile.defined,
            &after.tuning_profile.defined,
        )
        && project_selected(
            &original.tuning_profile.value,
            &without.tuning_profile.value,
            &after.tuning_profile.value,
        )
        && project_selected(
            &original.tuning_profile.parent,
            &without.tuning_profile.parent,
            &after.tuning_profile.parent,
        )
        && CLASS_FIELDS.into_iter().all(|key| {
            match (
                original.fields.get(key),
                without.fields.get(key),
                after.fields.get(key),
            ) {
                (Some(original), Some(without), Some(after)) => {
                    project_selected(&original.defined, &without.defined, &after.defined)
                        && project_selected(&original.value_nm, &without.value_nm, &after.value_nm)
                        && project_selected(&original.parent, &without.parent, &after.parent)
                }
                _ => false,
            }
        })
}

pub(in crate::schematic_audit::synchronization) fn verify_project_endpoint_netclasses(
    original: &NativeReport,
    without: &NativeReport,
    after: &NativeReport,
    replacement_pads: &BTreeMap<String, String>,
    assignment_changes: &BTreeSet<String>,
) -> Result<(), String> {
    // Both candidate projections must represent the exact same PCB with only its project changed.
    if without.native_version != after.native_version
        || without.board_sha256 != after.board_sha256
        || without.native_saved_sha256 != after.native_saved_sha256
        || without.footprints != after.footprints
        || without.pads != after.pads
        || without.copper != after.copper
        || without.netclasses.keys().ne(after.netclasses.keys())
    {
        return Err("project class authority requires identical candidate PCB projections".into());
    }
    let next = after
        .pads
        .iter()
        .map(|pad| (&pad.uuid, &pad.net))
        .chain(after.copper.iter().map(|item| (&item.uuid, &item.net)))
        .collect::<BTreeMap<_, _>>();
    for (id, net) in original
        .pads
        .iter()
        .map(|pad| (&pad.uuid, &pad.net))
        .chain(original.copper.iter().map(|item| (&item.uuid, &item.net)))
    {
        let target = replacement_pads.get(id).unwrap_or(id);
        let Some(target_net) = next.get(target) else {
            continue;
        };
        if net.is_empty() || target_net.is_empty() {
            continue;
        }
        if replacement_pads.contains_key(id) {
            let old = original
                .pads
                .iter()
                .find(|pad| &pad.uuid == id)
                .ok_or("missing replaced class endpoint")?;
            let new = after
                .pads
                .iter()
                .find(|pad| &pad.uuid == target)
                .ok_or("missing replacement class endpoint")?;
            if old.number != new.number {
                return Err("project rule transition changed replacement pad number".into());
            }
        }
        let old = original
            .netclasses
            .get(net)
            .ok_or("missing original endpoint netclass")?;
        let control = without
            .netclasses
            .get(*target_net)
            .ok_or("missing original-project candidate netclass")?;
        let actual = after
            .netclasses
            .get(*target_net)
            .ok_or("missing candidate-project netclass")?;
        if !class_transition_allowed(
            old,
            control,
            actual,
            assignment_changes.contains(*target_net),
        ) {
            return Err(format!(
                "synchronization_project_edit_does_not_authorize_endpoint_class_drift: {id}: {} -> {}",
                old.name, actual.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn class(name: &str, clearance: i64) -> NativeEffectiveClass {
        let identity = NativeClassIdentity {
            name: name.into(),
            priority: 0,
        };
        NativeEffectiveClass {
            name: name.into(),
            priority: 0,
            description: String::new(),
            tuning_profile: NativeClassText {
                defined: false,
                value: String::new(),
                parent: Some(identity.clone()),
            },
            constituents: vec![identity.clone()],
            fields: CLASS_FIELDS
                .into_iter()
                .map(|key| {
                    (
                        key.into(),
                        NativeClassDimension {
                            defined: true,
                            value_nm: clearance,
                            parent: Some(identity.clone()),
                        },
                    )
                })
                .collect(),
        }
    }
    #[test]
    fn unused_or_partial_project_effects_cannot_hide_unrelated_endpoint_rule_loss() {
        let original = class("Power", 420000);
        let default = class("Default", 200000);
        assert!(!class_transition_allowed(
            &original, &default, &default, false
        ));
        let mut partial = default.clone();
        partial.fields.get_mut("Clearance").unwrap().value_nm += 1;
        assert!(!class_transition_allowed(
            &original, &default, &partial, false
        ));
        assert!(class_transition_allowed(
            &original, &default, &partial, true
        ));
        assert!(class_transition_allowed(
            &original, &default, &original, false
        ));
        let mut valid = original.clone();
        valid.fields.get_mut("Clearance").unwrap().value_nm += 1;
        assert!(class_transition_allowed(
            &original, &original, &valid, false
        ));
        // A same-name inherited parameter drift remains protected when another field changes.
        let mut drift = valid.clone();
        drift.fields.get_mut("TrackWidth").unwrap().value_nm += 10;
        assert!(!class_transition_allowed(&drift, &original, &valid, false));
    }
}
