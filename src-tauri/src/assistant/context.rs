use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use serde_json::Value;

use super::session::Memory;
use crate::home_assistant::model::{Entity, Home};

const MAX_CONTEXT_ENTITIES: usize = 15;
const MAX_PREVIOUS_STATES: usize = 10;
/// Larger reads would cost more prompt time than asking the model to narrow them.
const MAX_STATE_RESULTS: usize = 25;

const STOP_WORDS: &[&str] = &[
    "the",
    "and",
    "all",
    "any",
    "are",
    "every",
    "everything",
    "except",
    "for",
    "from",
    "have",
    "into",
    "its",
    "off",
    "please",
    "room",
    "still",
    "that",
    "this",
    "turn",
    "what",
    "with",
];

/// Words that point at a device type rather than a name.
const DOMAIN_WORDS: &[(&str, &[&str])] = &[
    (
        "light",
        &["light", "lamp", "bright", "dark", "dim", "bed", "sleep"],
    ),
    ("switch", &["switch", "plug", "outlet"]),
    ("fan", &["fan"]),
    (
        "cover",
        &[
            "blind", "curtain", "shade", "shutter", "garage", "gate", "cover", "open", "close",
        ],
    ),
    (
        "climate",
        &[
            "thermostat",
            "heating",
            "heat",
            "cool",
            "cooling",
            "warm",
            "cold",
            "temperature",
            "comfortable",
        ],
    ),
    ("sensor", &["temperature", "humidity", "power", "energy"]),
    ("lock", &["lock", "unlock", "door"]),
    (
        "media_player",
        &["tv", "television", "speaker", "music", "media"],
    ),
    (
        "scene",
        &["scene", "mode", "bed", "movie", "night", "morning"],
    ),
];

/// Floors and areas. Changes only with Home Assistant's registries, so the model can cache it.
pub fn layout(home: &Home) -> String {
    let mut text = String::from("Floors:\n");
    for floor in &home.floors {
        let _ = writeln!(
            text,
            "- {} [{}]{}",
            floor.name,
            floor.id,
            aliases(&floor.aliases)
        );
    }
    text.push_str("Areas:\n");
    for area in &home.areas {
        let floor = area
            .floor_id
            .as_deref()
            .and_then(|id| home.floor(id))
            .map(|floor| format!(", floor {}", floor.id))
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "- {} [{}]{floor}{}",
            area.name,
            area.id,
            aliases(&area.aliases)
        );
    }
    text
}

/// Current facts for one request. Remembered entities are listed with their live state.
pub fn request_context(home: &Home, memory: &Memory, request: &str, time: Option<&str>) -> String {
    let referenced: Vec<&Entity> = memory
        .referenced
        .iter()
        .filter_map(|reference| home.entity(&reference.id))
        .collect();
    let mut text = String::new();
    if !referenced.is_empty() {
        text.push_str("Recently referenced, current states (\"it\" and \"that\" mean these):\n");
        for entity in &referenced {
            text.push_str(&describe(home, entity));
        }
    }
    let refers_back = refers_back(request);
    // Earlier states only matter when the request is about what just happened.
    if refers_back && !memory.last_action.is_empty() {
        text.push_str("States before the last action:\n");
        for entity in memory.last_action.iter().take(MAX_PREVIOUS_STATES) {
            let _ = writeln!(
                text,
                "- {} [{}]: {}",
                entity.name,
                entity.id,
                details(entity)
            );
        }
        if memory.last_action.len() > MAX_PREVIOUS_STATES {
            let hidden = memory.last_action.len() - MAX_PREVIOUS_STATES;
            let _ = writeln!(text, "{hidden} more not shown.");
        }
    }
    let expand_kinds = referenced.is_empty() || !refers_back;
    let relevant: Vec<&Entity> = relevant_entities(home, request, expand_kinds)
        .into_iter()
        .filter(|entity| !referenced.iter().any(|known| known.id == entity.id))
        .collect();
    if !relevant.is_empty() {
        text.push_str("Possibly relevant, current states (use get_states for others):\n");
        for entity in relevant {
            text.push_str(&describe(home, entity));
        }
    }
    if let Some(time) = time {
        let _ = writeln!(text, "Time from Home Assistant: {time}");
    }
    text
}

/// States for a read. Too many matches are summarised by type, never shown as a partial list.
pub fn describe_states(home: &Home, entities: &[&Entity]) -> String {
    if entities.is_empty() {
        return "No matching entities.".into();
    }
    if entities.len() > MAX_STATE_RESULTS {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for entity in entities {
            *counts.entry(entity.domain()).or_default() += 1;
        }
        let counts: Vec<String> = counts
            .iter()
            .map(|(domain, count)| format!("{domain} {count}"))
            .collect();
        return format!(
            "{} entities match ({}). Nothing listed. Narrow the target with domains, areas, device_classes, or entities.",
            entities.len(),
            counts.join(", ")
        );
    }
    entities
        .iter()
        .map(|entity| describe(home, entity))
        .collect()
}

fn describe(home: &Home, entity: &Entity) -> String {
    let area = entity
        .area_id
        .as_deref()
        .and_then(|id| home.area(id))
        .map(|area| format!(" in {}", area.name))
        .unwrap_or_default();
    format!(
        "- {} [{}]{area}: {}\n",
        entity.name,
        entity.id,
        details(entity)
    )
}

fn details(entity: &Entity) -> String {
    let mut details = vec![entity.state.clone()];
    if let Some(unit) = entity
        .attributes
        .get("unit_of_measurement")
        .and_then(Value::as_str)
    {
        details[0].push_str(unit);
    }
    if let Some(class) = entity.device_class() {
        details.push(format!("class {class}"));
    }
    if let Some(brightness) = entity.attributes.get("brightness").and_then(Value::as_f64) {
        details.push(format!(
            "brightness {}%",
            (brightness * 100.0 / 255.0).round()
        ));
    }
    for (key, label) in [
        ("current_temperature", "current"),
        ("temperature", "target"),
    ] {
        if let Some(value) = entity.attributes.get(key).and_then(Value::as_f64) {
            details.push(format!("{label} {value}°"));
        }
    }
    details.join(", ")
}

fn aliases(aliases: &[String]) -> String {
    if aliases.is_empty() {
        String::new()
    } else {
        format!(" (also called {})", aliases.join(", "))
    }
}

/// Entities a request may be about. `expand_kinds` lists every device of a mentioned kind
/// when no place is named; it is off when the request refers to remembered entities.
fn relevant_entities<'a>(home: &'a Home, request: &str, expand_kinds: bool) -> Vec<&'a Entity> {
    let words = keywords(request);
    if words.is_empty() {
        return Vec::new();
    }
    let domains: HashSet<&str> = DOMAIN_WORDS
        .iter()
        .filter(|(_, triggers)| triggers.iter().any(|trigger| words.contains(*trigger)))
        .map(|(domain, _)| *domain)
        .collect();
    // "Make the house comfortable" names no place, so every device of the kind matters.
    let names_a_place = home
        .areas
        .iter()
        .any(|area| overlap(&words, &area.name) > 0)
        || home
            .floors
            .iter()
            .any(|floor| overlap(&words, &floor.name) > 0);

    // Names often repeat their room ("living room blinds"). When a kind of device is named,
    // place words select through the area and kind instead of matching every name.
    let name_words: HashSet<String> = if domains.is_empty() {
        words.clone()
    } else {
        let place_words: HashSet<String> = home
            .areas
            .iter()
            .map(|area| area.name.as_str())
            .chain(home.floors.iter().map(|floor| floor.name.as_str()))
            .flat_map(keywords)
            .collect();
        words.difference(&place_words).cloned().collect()
    };

    let mut scored: Vec<(usize, &Entity)> = home
        .entities
        .values()
        .filter(|entity| !entity.internal)
        .filter_map(|entity| {
            let name_score = overlap(&name_words, &entity.name) * 3
                + entity
                    .aliases
                    .iter()
                    .map(|alias| overlap(&name_words, alias) * 3)
                    .sum::<usize>();
            let area_score = entity
                .area_id
                .as_deref()
                .and_then(|id| home.area(id))
                .map_or(0, |area| overlap(&words, &area.name));
            let domain_match = domains.contains(entity.domain());
            let include = name_score > 0
                || area_score > 0 && (domains.is_empty() || domain_match)
                || expand_kinds && !names_a_place && domain_match;
            let score = name_score + area_score + usize::from(domain_match);
            include.then_some((score, entity))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    scored
        .into_iter()
        .take(MAX_CONTEXT_ENTITIES)
        .map(|(_, entity)| entity)
        .collect()
}

fn refers_back(request: &str) -> bool {
    const BACK_WORDS: &[&str] = &[
        "it", "that", "them", "those", "these", "this", "they", "undo", "revert", "back", "before",
        "previous", "again",
    ];
    // Apostrophes stay inside words so "it's too bright" does not count as "it".
    request
        .to_lowercase()
        .replace('\u{2019}', "'")
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .any(|word| BACK_WORDS.contains(&word))
}

fn keywords(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 2 && !STOP_WORDS.contains(word))
        .map(|word| {
            word.strip_suffix('s')
                .filter(|stem| stem.len() >= 3)
                .unwrap_or(word)
                .to_owned()
        })
        .collect()
}

fn overlap(words: &HashSet<String>, text: &str) -> usize {
    keywords(text).intersection(words).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home_assistant::model::fixtures::home;

    fn ids(home: &Home, request: &str) -> Vec<String> {
        relevant_entities(home, request, true)
            .into_iter()
            .map(|entity| entity.id.clone())
            .collect()
    }

    #[test]
    fn layout_lists_floors_and_areas() {
        let layout = layout(&home());
        assert!(layout.contains("- Downstairs [downstairs]"));
        assert!(layout.contains("- Kitchen [kitchen], floor downstairs"));
    }

    #[test]
    fn request_context_shows_live_states_for_remembered_entities() {
        use crate::assistant::session::Turn;
        use crate::home_assistant::model::fixtures::state;

        let mut home = home();
        let mut memory = Memory::default();
        let mut turn = Turn::default();
        turn.refer("light.kitchen", "on".into());
        turn.previous
            .push(home.entity("light.kitchen").unwrap().clone());
        memory.record(turn);
        home.apply_state(
            "light.kitchen",
            Some(state("light.kitchen", "off", serde_json::json!({}))),
        );

        let context = request_context(&home, &memory, "hello", None);
        assert!(context.contains("Recently referenced"));
        assert!(context.contains("- kitchen [light.kitchen] in Kitchen: off\n"));
        assert!(!context.contains("States before the last action"));
        let context = request_context(&home, &memory, "It's too dark", None);
        assert!(!context.contains("States before the last action"));
        let context = request_context(&home, &memory, "put it back", None);
        assert!(context.contains("States before the last action:\n- kitchen [light.kitchen]: on"));
        assert!(!context.contains("Time from Home Assistant"));
        assert!(
            request_context(&home, &memory, "time?", Some("5:19 PM"))
                .contains("Time from Home Assistant: 5:19 PM")
        );
    }

    #[test]
    fn matches_entities_by_name() {
        let found = ids(
            &home(),
            "Turn off everything downstairs except the hallway light",
        );
        assert_eq!(found.first().map(String::as_str), Some("light.hallway"));
    }

    #[test]
    fn matches_area_and_device_type() {
        let found = ids(&home(), "It's too bright in the living room");
        assert!(found.contains(&"light.living_room_lamp".to_string()));
        assert!(!found.contains(&"light.bedroom".to_string()));
    }

    #[test]
    fn room_names_inside_entity_names_do_not_pull_in_other_kinds() {
        let found = ids(&home(), "It's too bright in the living room");
        assert_eq!(found, ["light.living_room_lamp"]);
    }

    #[test]
    fn requests_without_a_place_include_devices_of_the_mentioned_kind() {
        let found = ids(&home(), "Make the house comfortable");
        assert!(found.contains(&"climate.thermostat".to_string()));
        let found = ids(&home(), "It's too bright in the living room");
        assert!(!found.contains(&"light.bedroom".to_string()));
    }

    #[test]
    fn never_includes_internal_entities() {
        let found = ids(&home(), "hidden relay child lock");
        assert!(
            !found
                .iter()
                .any(|id| id.starts_with("switch.hidden") || id.contains("child"))
        );
    }

    #[test]
    fn oversized_reads_are_summarised_instead_of_truncated() {
        let home = crate::home_assistant::model::fixtures::large_home(40);
        let entities: Vec<&Entity> = home.entities.values().collect();
        let text = describe_states(&home, &entities);
        assert!(text.starts_with(&format!("{} entities match (", entities.len())));
        assert!(text.contains("light 14"));
        assert!(!text.contains('['));
    }

    #[test]
    fn state_descriptions_are_compact() {
        let home = home();
        let thermostat = home.entity("climate.thermostat").unwrap();
        assert_eq!(
            describe_states(&home, &[thermostat]),
            "- thermostat [climate.thermostat] in Hallway: heat, current 19.5°, target 20°\n"
        );
    }
}
