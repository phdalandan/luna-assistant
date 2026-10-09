use std::collections::HashSet;
use std::fmt::Write;

use serde_json::Value;

use crate::home_assistant::model::{Entity, Home};

const MAX_CONTEXT_ENTITIES: usize = 25;
const MAX_STATE_RESULTS: usize = 60;

const STOP_WORDS: &[&str] = &[
    "the", "and", "all", "any", "are", "every", "everything", "except", "for", "from", "have",
    "into", "its", "off", "please", "room", "still", "that", "this", "turn", "what", "with",
];

/// Words that point at a device type rather than a name.
const DOMAIN_WORDS: &[(&str, &[&str])] = &[
    ("light", &["light", "lamp", "bright", "dark", "dim"]),
    ("switch", &["switch", "plug", "outlet"]),
    ("fan", &["fan"]),
    (
        "cover",
        &["blind", "curtain", "shade", "shutter", "garage", "gate", "cover", "open", "close"],
    ),
    (
        "climate",
        &["thermostat", "heating", "heat", "cool", "cooling", "warm", "cold", "temperature"],
    ),
    ("sensor", &["temperature", "humidity", "power", "energy"]),
    ("lock", &["lock", "unlock", "door"]),
    ("media_player", &["tv", "television", "speaker", "music", "media"]),
    ("scene", &["scene", "mode", "bed", "movie", "night", "morning"]),
];

/// Floors, areas, and the entities most likely to matter for `request`.
pub fn summarize(home: &Home, request: &str) -> String {
    let mut text = String::from("Floors:\n");
    for floor in &home.floors {
        let _ = writeln!(text, "- {} [{}]{}", floor.name, floor.id, aliases(&floor.aliases));
    }
    text.push_str("Areas:\n");
    for area in &home.areas {
        let floor = area
            .floor_id
            .as_deref()
            .and_then(|id| home.floor(id))
            .map(|floor| format!(", floor {}", floor.id))
            .unwrap_or_default();
        let _ = writeln!(text, "- {} [{}]{floor}{}", area.name, area.id, aliases(&area.aliases));
    }
    let relevant = relevant_entities(home, request);
    if !relevant.is_empty() {
        text.push_str("Possibly relevant entities (use get_states for others):\n");
        for entity in relevant {
            text.push_str(&describe(home, entity));
        }
    }
    text
}

pub fn describe_states(home: &Home, entities: &[&Entity]) -> String {
    if entities.is_empty() {
        return "No matching entities.".into();
    }
    let mut text = String::new();
    for entity in entities.iter().take(MAX_STATE_RESULTS) {
        text.push_str(&describe(home, entity));
    }
    if entities.len() > MAX_STATE_RESULTS {
        let hidden = entities.len() - MAX_STATE_RESULTS;
        let _ = writeln!(text, "{hidden} more not shown. Narrow the target.");
    }
    text
}

fn describe(home: &Home, entity: &Entity) -> String {
    let area = entity
        .area_id
        .as_deref()
        .and_then(|id| home.area(id))
        .map(|area| format!(" in {}", area.name))
        .unwrap_or_default();
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
        details.push(format!("brightness {}%", (brightness * 100.0 / 255.0).round()));
    }
    for (key, label) in [("current_temperature", "current"), ("temperature", "target")] {
        if let Some(value) = entity.attributes.get(key).and_then(Value::as_f64) {
            details.push(format!("{label} {value}°"));
        }
    }
    format!("- {} [{}]{area}: {}\n", entity.name, entity.id, details.join(", "))
}

fn aliases(aliases: &[String]) -> String {
    if aliases.is_empty() {
        String::new()
    } else {
        format!(" (also called {})", aliases.join(", "))
    }
}

fn relevant_entities<'a>(home: &'a Home, request: &str) -> Vec<&'a Entity> {
    let words = keywords(request);
    if words.is_empty() {
        return Vec::new();
    }
    let domains: HashSet<&str> = DOMAIN_WORDS
        .iter()
        .filter(|(_, triggers)| triggers.iter().any(|trigger| words.contains(*trigger)))
        .map(|(domain, _)| *domain)
        .collect();

    let mut scored: Vec<(usize, &Entity)> = home
        .entities
        .values()
        .filter(|entity| !entity.internal)
        .filter_map(|entity| {
            let name_score = overlap(&words, &entity.name) * 3
                + entity
                    .aliases
                    .iter()
                    .map(|alias| overlap(&words, alias) * 3)
                    .sum::<usize>();
            let area_score = entity
                .area_id
                .as_deref()
                .and_then(|id| home.area(id))
                .map_or(0, |area| overlap(&words, &area.name));
            let domain_match = domains.contains(entity.domain());
            let include = name_score > 0 || area_score > 0 && (domains.is_empty() || domain_match);
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
        relevant_entities(home, request)
            .into_iter()
            .map(|entity| entity.id.clone())
            .collect()
    }

    #[test]
    fn summary_lists_floors_and_areas() {
        let summary = summarize(&home(), "hello");
        assert!(summary.contains("- Downstairs [downstairs]"));
        assert!(summary.contains("- Kitchen [kitchen], floor downstairs"));
        assert!(!summary.contains("Possibly relevant"));
    }

    #[test]
    fn matches_entities_by_name() {
        let found = ids(&home(), "Turn off everything downstairs except the hallway light");
        assert_eq!(found.first().map(String::as_str), Some("light.hallway"));
    }

    #[test]
    fn matches_area_and_device_type() {
        let found = ids(&home(), "It's too bright in the living room");
        assert!(found.contains(&"light.living_room_lamp".to_string()));
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
    fn state_descriptions_are_compact() {
        let home = home();
        let thermostat = home.entity("climate.thermostat").unwrap();
        assert_eq!(
            describe_states(&home, &[thermostat]),
            "- thermostat [climate.thermostat] in Hallway: heat, current 19.5°, target 20°\n"
        );
    }
}
