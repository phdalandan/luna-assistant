use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize)]
pub struct FloorEntry {
    pub floor_id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<Option<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AreaEntry {
    pub area_id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<Option<String>>,
    pub floor_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceEntry {
    pub id: String,
    pub area_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntityEntry {
    pub entity_id: String,
    pub name: Option<String>,
    pub area_id: Option<String>,
    pub device_id: Option<String>,
    #[serde(default)]
    pub aliases: Vec<Option<String>>,
    pub hidden_by: Option<String>,
    pub entity_category: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateEntry {
    pub entity_id: String,
    pub state: String,
    #[serde(default)]
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Default)]
pub struct Registries {
    pub floors: Vec<FloorEntry>,
    pub areas: Vec<AreaEntry>,
    pub devices: Vec<DeviceEntry>,
    pub entities: Vec<EntityEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Floor {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub floor_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub area_id: Option<String>,
    pub state: String,
    pub attributes: Map<String, Value>,
    /// Hidden or configuration/diagnostic entities are never part of broad selections.
    pub internal: bool,
}

impl Entity {
    pub fn domain(&self) -> &str {
        self.id.split_once('.').map_or("", |(domain, _)| domain)
    }

    pub fn device_class(&self) -> Option<&str> {
        self.attributes.get("device_class").and_then(Value::as_str)
    }

    pub fn supported_features(&self) -> u64 {
        self.attributes
            .get("supported_features")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    pub fn is_available(&self) -> bool {
        !matches!(self.state.as_str(), "unavailable" | "unknown")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Home {
    pub floors: Vec<Floor>,
    pub areas: Vec<Area>,
    pub entities: BTreeMap<String, Entity>,
    entity_registry: HashMap<String, EntityEntry>,
    device_areas: HashMap<String, String>,
}

impl Home {
    pub fn build(registries: Registries, states: Vec<StateEntry>) -> Self {
        let mut home = Self {
            floors: registries
                .floors
                .into_iter()
                .map(|floor| Floor {
                    id: floor.floor_id,
                    name: floor.name,
                    aliases: flatten(floor.aliases),
                })
                .collect(),
            areas: registries
                .areas
                .into_iter()
                .map(|area| Area {
                    id: area.area_id,
                    name: area.name,
                    aliases: flatten(area.aliases),
                    floor_id: area.floor_id,
                })
                .collect(),
            entities: BTreeMap::new(),
            entity_registry: registries
                .entities
                .into_iter()
                .map(|entry| (entry.entity_id.clone(), entry))
                .collect(),
            device_areas: registries
                .devices
                .into_iter()
                .filter_map(|device| Some((device.id, device.area_id?)))
                .collect(),
        };
        for state in states {
            home.apply_state(&state.entity_id.clone(), Some(state));
        }
        home
    }

    /// Applies a `state_changed` event. `None` means the entity was removed.
    pub fn apply_state(&mut self, entity_id: &str, state: Option<StateEntry>) {
        let Some(state) = state else {
            self.entities.remove(entity_id);
            return;
        };
        let entity = self.entity_from_state(state);
        self.entities.insert(entity.id.clone(), entity);
    }

    fn entity_from_state(&self, state: StateEntry) -> Entity {
        let entry = self.entity_registry.get(&state.entity_id);
        let area_id = entry.and_then(|entry| {
            entry.area_id.clone().or_else(|| {
                let device_id = entry.device_id.as_ref()?;
                self.device_areas.get(device_id).cloned()
            })
        });
        let name = state
            .attributes
            .get("friendly_name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| entry.and_then(|entry| entry.name.clone()))
            .unwrap_or_else(|| state.entity_id.clone());
        Entity {
            name,
            aliases: entry
                .map(|entry| flatten(entry.aliases.clone()))
                .unwrap_or_default(),
            area_id,
            internal: entry
                .is_some_and(|entry| entry.hidden_by.is_some() || entry.entity_category.is_some()),
            id: state.entity_id,
            state: state.state,
            attributes: state.attributes,
        }
    }

    pub fn entity(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)
    }

    pub fn area(&self, id: &str) -> Option<&Area> {
        self.areas.iter().find(|area| area.id == id)
    }

    pub fn floor(&self, id: &str) -> Option<&Floor> {
        self.floors.iter().find(|floor| floor.id == id)
    }

    pub fn floor_id_of(&self, entity: &Entity) -> Option<&str> {
        let area = self.area(entity.area_id.as_deref()?)?;
        area.floor_id.as_deref()
    }
}

fn flatten(aliases: Vec<Option<String>>) -> Vec<String> {
    aliases.into_iter().flatten().collect()
}

#[cfg(test)]
pub mod fixtures {
    use serde_json::json;

    use super::*;

    pub fn state(entity_id: &str, state: &str, attributes: Value) -> StateEntry {
        let mut attributes = attributes.as_object().cloned().unwrap_or_default();
        attributes
            .entry("friendly_name")
            .or_insert_with(|| json!(entity_id.split_once('.').unwrap().1.replace('_', " ")));
        StateEntry {
            entity_id: entity_id.into(),
            state: state.into(),
            attributes,
        }
    }

    fn entity_entry(entity_id: &str, area_id: Option<&str>, device_id: Option<&str>) -> EntityEntry {
        EntityEntry {
            entity_id: entity_id.into(),
            name: None,
            area_id: area_id.map(Into::into),
            device_id: device_id.map(Into::into),
            aliases: vec![],
            hidden_by: None,
            entity_category: None,
        }
    }

    /// Two floors, five areas, and a mix of controllable and sensitive entities.
    pub fn home() -> Home {
        let floor = |id: &str, name: &str| FloorEntry {
            floor_id: id.into(),
            name: name.into(),
            aliases: vec![],
        };
        let area = |id: &str, name: &str, floor: &str| AreaEntry {
            area_id: id.into(),
            name: name.into(),
            aliases: vec![],
            floor_id: Some(floor.into()),
        };
        let mut hidden = entity_entry("switch.hidden_relay", Some("living_room"), None);
        hidden.hidden_by = Some("user".into());
        let mut config = entity_entry("switch.kitchen_child_lock", Some("kitchen"), None);
        config.entity_category = Some("config".into());

        let registries = Registries {
            floors: vec![floor("downstairs", "Downstairs"), floor("upstairs", "Upstairs")],
            areas: vec![
                area("living_room", "Living Room", "downstairs"),
                area("kitchen", "Kitchen", "downstairs"),
                area("hallway", "Hallway", "downstairs"),
                area("garage", "Garage", "downstairs"),
                area("bedroom", "Bedroom", "upstairs"),
            ],
            devices: vec![DeviceEntry {
                id: "lamp_device".into(),
                area_id: Some("living_room".into()),
            }],
            entities: vec![
                entity_entry("light.living_room_lamp", None, Some("lamp_device")),
                entity_entry("light.kitchen", Some("kitchen"), None),
                entity_entry("light.hallway", Some("hallway"), None),
                entity_entry("light.bedroom", Some("bedroom"), None),
                entity_entry("switch.tv_plug", Some("living_room"), None),
                entity_entry("media_player.living_room_tv", Some("living_room"), None),
                entity_entry("climate.thermostat", Some("hallway"), None),
                entity_entry("lock.front_door", Some("hallway"), None),
                entity_entry("cover.garage_door", Some("garage"), None),
                entity_entry("cover.living_room_blinds", Some("living_room"), None),
                entity_entry("sensor.bedroom_temperature", Some("bedroom"), None),
                entity_entry("scene.movie_night", None, None),
                hidden,
                config,
            ],
        };
        let states = vec![
            state("light.living_room_lamp", "on", json!({"supported_color_modes": ["brightness"], "brightness": 255})),
            state("light.kitchen", "on", json!({"supported_color_modes": ["onoff"]})),
            state("light.hallway", "on", json!({"supported_color_modes": ["brightness"], "brightness": 128})),
            state("light.bedroom", "off", json!({"supported_color_modes": ["brightness"]})),
            state("switch.tv_plug", "on", json!({})),
            state("media_player.living_room_tv", "playing", json!({})),
            state("climate.thermostat", "heat", json!({"supported_features": 1, "temperature": 20, "current_temperature": 19.5, "min_temp": 7, "max_temp": 35})),
            state("lock.front_door", "locked", json!({})),
            state("cover.garage_door", "closed", json!({"device_class": "garage", "supported_features": 3})),
            state("cover.living_room_blinds", "open", json!({"device_class": "blind", "supported_features": 3})),
            state("sensor.bedroom_temperature", "18.2", json!({"device_class": "temperature", "unit_of_measurement": "°C"})),
            state("scene.movie_night", "2026-01-01T00:00:00+00:00", json!({})),
            state("switch.hidden_relay", "on", json!({})),
            state("switch.kitchen_child_lock", "off", json!({})),
        ];
        Home::build(registries, states)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::fixtures::{home, state};

    #[test]
    fn entity_area_falls_back_to_device_area() {
        let home = home();
        let lamp = home.entity("light.living_room_lamp").unwrap();
        assert_eq!(lamp.area_id.as_deref(), Some("living_room"));
        assert_eq!(home.floor_id_of(lamp), Some("downstairs"));
    }

    #[test]
    fn hidden_and_config_entities_are_internal() {
        let home = home();
        assert!(home.entity("switch.hidden_relay").unwrap().internal);
        assert!(home.entity("switch.kitchen_child_lock").unwrap().internal);
        assert!(!home.entity("switch.tv_plug").unwrap().internal);
    }

    #[test]
    fn state_changes_keep_registry_metadata() {
        let mut home = home();
        home.apply_state("light.kitchen", Some(state("light.kitchen", "off", json!({}))));
        let kitchen = home.entity("light.kitchen").unwrap();
        assert_eq!(kitchen.state, "off");
        assert_eq!(kitchen.area_id.as_deref(), Some("kitchen"));
    }

    #[test]
    fn removed_entities_are_dropped() {
        let mut home = home();
        home.apply_state("light.kitchen", None);
        assert!(home.entity("light.kitchen").is_none());
    }

    #[test]
    fn entities_without_registry_entries_have_no_area() {
        let mut home = home();
        home.apply_state("light.yaml_light", Some(state("light.yaml_light", "on", json!({}))));
        assert_eq!(home.entity("light.yaml_light").unwrap().area_id, None);
    }
}
