use std::collections::BTreeMap;

use super::Target;
use crate::home_assistant::model::{Entity, Home};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("unknown floor id: {0}")]
    UnknownFloor(String),
    #[error("unknown area id: {0}")]
    UnknownArea(String),
    #[error("unknown entity id: {0}")]
    UnknownEntity(String),
    #[error("the target is empty; name entities, areas, floors, domains, or set everywhere")]
    EmptyTarget,
}

#[derive(Debug, Clone, Copy)]
pub struct Selected<'a> {
    pub entity: &'a Entity,
    /// Named directly by ID rather than matched through a floor, area, or domain.
    pub explicit: bool,
}

#[derive(Debug, Default)]
pub struct Selection<'a> {
    pub entities: Vec<Selected<'a>>,
}

/// Resolves a target to entities. Exclusions are applied here, before anything is planned.
pub fn select<'a>(home: &'a Home, target: &Target) -> Result<Selection<'a>, ResolveError> {
    check_references(home, target)?;
    let has_location = target.everywhere || !target.floors.is_empty() || !target.areas.is_empty();
    if !has_location && target.entities.is_empty() && target.domains.is_empty() {
        return Err(ResolveError::EmptyTarget);
    }

    let mut selected: BTreeMap<&str, Selected<'a>> = BTreeMap::new();
    if has_location || !target.domains.is_empty() {
        for entity in home.entities.values() {
            if !entity.internal
                && in_location(home, target, entity)
                && matches_filters(target, entity)
            {
                selected.insert(
                    &entity.id,
                    Selected {
                        entity,
                        explicit: false,
                    },
                );
            }
        }
    }
    for id in &target.entities {
        if let Some(entity) = home.entity(id) {
            selected.insert(
                &entity.id,
                Selected {
                    entity,
                    explicit: true,
                },
            );
        }
    }

    selected.retain(|_, selected| !is_excluded(target, selected.entity));
    Ok(Selection {
        entities: selected.into_values().collect(),
    })
}

fn check_references(home: &Home, target: &Target) -> Result<(), ResolveError> {
    if let Some(id) = target.floors.iter().find(|id| home.floor(id).is_none()) {
        return Err(ResolveError::UnknownFloor(id.clone()));
    }
    let mut areas = target.areas.iter().chain(&target.exclude_areas);
    if let Some(id) = areas.find(|id| home.area(id).is_none()) {
        return Err(ResolveError::UnknownArea(id.clone()));
    }
    let mut entities = target.entities.iter().chain(&target.exclude_entities);
    if let Some(id) = entities.find(|id| home.entity(id).is_none()) {
        return Err(ResolveError::UnknownEntity(id.clone()));
    }
    Ok(())
}

fn in_location(home: &Home, target: &Target, entity: &Entity) -> bool {
    if target.everywhere || (target.floors.is_empty() && target.areas.is_empty()) {
        return true;
    }
    let in_area = entity
        .area_id
        .as_ref()
        .is_some_and(|area| target.areas.contains(area));
    let in_floor = home
        .floor_id_of(entity)
        .is_some_and(|floor| target.floors.iter().any(|id| id == floor));
    in_area || in_floor
}

fn matches_filters(target: &Target, entity: &Entity) -> bool {
    let domain_ok = target.domains.is_empty()
        || target
            .domains
            .iter()
            .any(|domain| domain == entity.domain());
    let class_ok = target.device_classes.is_empty()
        || entity
            .device_class()
            .is_some_and(|class| target.device_classes.iter().any(|wanted| wanted == class));
    domain_ok && class_ok
}

fn is_excluded(target: &Target, entity: &Entity) -> bool {
    target.exclude_entities.contains(&entity.id)
        || entity
            .area_id
            .as_ref()
            .is_some_and(|area| target.exclude_areas.contains(area))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home_assistant::model::fixtures::home;

    fn ids(home: &Home, target: Target) -> Vec<String> {
        select(home, &target)
            .unwrap()
            .entities
            .into_iter()
            .map(|selected| selected.entity.id.clone())
            .collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn floor_includes_entities_through_areas_and_devices() {
        let selected = ids(
            &home(),
            Target {
                floors: strings(&["downstairs"]),
                domains: strings(&["light"]),
                ..Target::default()
            },
        );
        assert_eq!(
            selected,
            ["light.hallway", "light.kitchen", "light.living_room_lamp"]
        );
    }

    #[test]
    fn explicit_exclusions_are_removed_before_planning() {
        let selected = ids(
            &home(),
            Target {
                floors: strings(&["downstairs"]),
                exclude_entities: strings(&["light.hallway"]),
                ..Target::default()
            },
        );
        assert!(!selected.contains(&"light.hallway".to_string()));
        assert!(selected.contains(&"light.kitchen".to_string()));
    }

    #[test]
    fn excluded_areas_are_removed() {
        let selected = ids(
            &home(),
            Target {
                floors: strings(&["downstairs"]),
                domains: strings(&["light"]),
                exclude_areas: strings(&["kitchen", "hallway"]),
                ..Target::default()
            },
        );
        assert_eq!(selected, ["light.living_room_lamp"]);
    }

    #[test]
    fn area_selection_stays_in_the_area() {
        let selected = ids(
            &home(),
            Target {
                areas: strings(&["bedroom"]),
                ..Target::default()
            },
        );
        assert_eq!(selected, ["light.bedroom", "sensor.bedroom_temperature"]);
    }

    #[test]
    fn domain_alone_means_everywhere() {
        let selected = ids(
            &home(),
            Target {
                domains: strings(&["light"]),
                ..Target::default()
            },
        );
        assert_eq!(selected.len(), 4);
    }

    #[test]
    fn device_class_filter_applies() {
        let selected = ids(
            &home(),
            Target {
                everywhere: true,
                device_classes: strings(&["temperature"]),
                ..Target::default()
            },
        );
        assert_eq!(selected, ["sensor.bedroom_temperature"]);
    }

    #[test]
    fn internal_entities_are_never_selected_broadly() {
        let selected = ids(
            &home(),
            Target {
                everywhere: true,
                ..Target::default()
            },
        );
        assert!(!selected.contains(&"switch.hidden_relay".to_string()));
        assert!(!selected.contains(&"switch.kitchen_child_lock".to_string()));
    }

    #[test]
    fn explicit_entities_are_marked_and_bypass_filters() {
        let home = home();
        let selection = select(
            &home,
            &Target {
                entities: strings(&["switch.tv_plug"]),
                areas: strings(&["kitchen"]),
                domains: strings(&["light"]),
                ..Target::default()
            },
        )
        .unwrap();
        let explicit: Vec<_> = selection
            .entities
            .iter()
            .filter(|selected| selected.explicit)
            .map(|selected| selected.entity.id.as_str())
            .collect();
        assert_eq!(explicit, ["switch.tv_plug"]);
        assert_eq!(selection.entities.len(), 2);
    }

    #[test]
    fn unknown_references_are_errors() {
        let home = home();
        let unknown_floor = Target {
            floors: strings(&["attic"]),
            ..Target::default()
        };
        assert_eq!(
            select(&home, &unknown_floor).err(),
            Some(ResolveError::UnknownFloor("attic".into()))
        );
        let unknown_exclusion = Target {
            everywhere: true,
            exclude_entities: strings(&["light.made_up"]),
            ..Target::default()
        };
        assert_eq!(
            select(&home, &unknown_exclusion).err(),
            Some(ResolveError::UnknownEntity("light.made_up".into()))
        );
    }

    #[test]
    fn empty_target_is_an_error() {
        assert_eq!(
            select(&home(), &Target::default()).err(),
            Some(ResolveError::EmptyTarget)
        );
    }
}
