//! Recognises simple, unambiguous requests so they skip the language model.
//! Anything this module cannot resolve to exact entities goes to the model instead.
use std::collections::HashSet;

use super::session::Memory;
use crate::actions::{self, Action};
use crate::home_assistant::model::{Entity, Home};

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Time,
    Undo,
    Recheck,
    Thanks,
    Control {
        action: Action,
        subject: Subject,
    },
    Query {
        asked: Asked,
        subject: Subject,
    },
    /// "What about the AC?", "and the hallway", "the hallway too": the previous request again,
    /// for something else.
    FollowUp {
        subject: Subject,
    },
    /// A brightness or temperature. Without a stated kind, the device decides which.
    Set {
        kind: Option<Setting>,
        value: f64,
        subject: Subject,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Brightness,
    Temperature,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Subject {
    /// "it", "that", "them": whatever the conversation last referred to.
    Pronoun,
    Named(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    Power,
    Opening,
    Locking,
    /// "Check the porch light", "garage status": any readable state.
    Status,
    /// "How warm is the bedroom?"
    Temperature,
}

/// Dropped from the start of a request.
const FILLER: &[&str] = &[
    "hey", "luna", "please", "can", "could", "would", "will", "you", "sorry", "oh", "okay", "ok",
    "wait", "actually", "um", "so", "just", "also", "now", "no", "nope", "nah",
];
/// Dropped from the end of a request.
const TRAILING: &[&str] = &[
    "please", "now", "luna", "again", "instead", "too", "thanks", "for", "me",
];
/// Words that add nothing to a device phrase ("turn it back on") unless a device is named with
/// them, as in "back porch".
const SOFT: &[&str] = &[
    "back", "again", "just", "also", "too", "instead", "actually",
];
const ARTICLES: &[&str] = &["the", "my", "our", "still", "in", "at", "of"];
const PRONOUNS: &[&str] = &["it", "that", "them", "those", "these", "this"];
const SETTING_VERBS: &[&str] = &["set", "dim", "change", "turn", "put", "make"];
const SETTING_WORDS: &[&str] = &["brightness", "temperature"];
/// Requests about several places or exceptions need the model's interpretation.
const BROAD: &[&str] = &[
    "all",
    "every",
    "everything",
    "except",
    "and",
    "or",
    "but",
    "everywhere",
    "house",
    "home",
];
const TIME_WORDS: &[&str] = &[
    "what", "what's", "whats", "is", "it", "the", "current", "now", "right", "tell", "me", "do",
    "know",
];
const UNDO_WORDS: &[&str] = &["that", "it", "this", "the", "last", "change", "action"];
const RECHECK: &[&str] = &[
    "are you sure",
    "you sure",
    "are you certain",
    "really",
    "double check",
    "check again",
    "is that right",
    "is that true",
];
/// Words that name a kind of device rather than a specific one.
const DEVICE_WORDS: &[(&str, &[&str])] = &[
    ("light", &["light", "switch"]),
    ("lamp", &["light", "switch"]),
    ("switch", &["switch"]),
    ("plug", &["switch"]),
    ("outlet", &["switch"]),
    ("fan", &["fan"]),
    ("blind", &["cover"]),
    ("shade", &["cover"]),
    ("curtain", &["cover"]),
    ("shutter", &["cover"]),
    ("door", &["cover", "lock", "binary_sensor"]),
    ("window", &["cover", "binary_sensor"]),
    ("lock", &["lock"]),
    ("tv", &["media_player"]),
    ("television", &["media_player"]),
    ("speaker", &["media_player"]),
    ("thermostat", &["climate"]),
    ("ac", &["climate"]),
    ("aircon", &["climate"]),
    ("air", &["climate", "fan"]),
    ("conditioner", &["climate"]),
    ("heating", &["climate"]),
];
const POWER_DOMAINS: &[&str] = &[
    "light",
    "switch",
    "fan",
    "input_boolean",
    "media_player",
    "climate",
    "binary_sensor",
];
const OPENING_DOMAINS: &[&str] = &["cover", "binary_sensor"];
const STATUS_DOMAINS: &[&str] = &[
    "light",
    "switch",
    "fan",
    "input_boolean",
    "media_player",
    "climate",
    "cover",
    "lock",
    "binary_sensor",
    "sensor",
];
/// A statement about a device's state, such as "it's on I think", is checked, never acted on.
const STATEMENT_STARTS: &[&str] = &[
    "it", "it's", "its", "that", "that's", "thats", "they", "they're",
];
const STATE_WORDS: &[&str] = &[
    "on", "off", "open", "closed", "locked", "unlocked", "wrong", "right", "true",
];
const ACTION_VERBS: &[&str] = &[
    "turn", "switch", "set", "open", "close", "shut", "lock", "unlock", "dim", "make", "put",
    "change", "start", "stop", "activate",
];
const STATUS_WORDS: &[&str] = &["check", "status", "state"];
const TEMPERATURE_WORDS: &[&str] = &["temperature", "warm", "cold", "hot"];
const TEMPERATURE_FILLER: &[&str] = &[
    "what",
    "what's",
    "whats",
    "is",
    "the",
    "how",
    "temperature",
    "warm",
    "cold",
    "hot",
    "room",
    "inside",
    "current",
    "currently",
    "reading",
    "there",
    "on",
    "for",
    "by",
];
/// Temperature questions read climate devices and temperature sensors only.
pub const TEMPERATURE_DOMAINS: &[&str] = &["climate", "sensor.temperature"];
/// A sentence with any of these is not just a device name, so it is never a follow-up.
const NOT_A_SUBJECT: &[&str] = &[
    "what",
    "why",
    "how",
    "when",
    "where",
    "who",
    "which",
    "i",
    "i'm",
    "im",
    "we",
    "we're",
    "is",
    "are",
    "was",
    "were",
    "do",
    "does",
    "did",
    "should",
    "goodnight",
    "night",
    "morning",
];
const MAX_FOLLOW_UP_WORDS: usize = 4;
const STATUS_FILLER: &[&str] = &[
    "check", "status", "state", "what", "what's", "whats", "is", "the", "of", "for", "show", "me",
    "tell", "current",
];
const CONTROL_DOMAINS: &[&str] = &[
    "light",
    "switch",
    "fan",
    "input_boolean",
    "media_player",
    "climate",
    "cover",
    "lock",
];

pub fn parse(request: &str) -> Option<Intent> {
    // "Oh wait. Revert that." is about its last sentence.
    let mut sentences: Vec<&str> = split_sentences(request)
        .into_iter()
        .filter(|sentence| !sentence.trim().is_empty())
        .collect();
    let last = sentences.pop()?;
    if sentences.iter().any(|sentence| tokens(sentence).len() > 2) {
        return None;
    }
    parse_sentence(last)
}

/// Splits at sentence punctuation, keeping decimal points in numbers like 21.5.
fn split_sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut sentences = Vec::new();
    let mut start = 0;
    for (index, c) in text.char_indices() {
        let decimal = c == '.'
            && index > 0
            && bytes[index - 1].is_ascii_digit()
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
        if matches!(c, '.' | '!' | '?' | ';') && !decimal {
            sentences.push(&text[start..index]);
            start = index + 1;
        }
    }
    sentences.push(&text[start..]);
    sentences
}

fn parse_sentence(sentence: &str) -> Option<Intent> {
    let tokens = tokens(sentence);
    let text = tokens.join(" ");
    let content: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|word| !TIME_WORDS.contains(word))
        .collect();
    if content == ["time"] {
        return Some(Intent::Time);
    }
    let words: Vec<&str> = tokens.iter().map(String::as_str).collect();
    if RECHECK.contains(&text.as_str()) || states_a_fact(&words) {
        return Some(Intent::Recheck);
    }
    if let ["thanks" | "thank" | "cheers", rest @ ..] = words.as_slice()
        && rest
            .iter()
            .all(|word| matches!(*word, "you" | "so" | "much" | "luna"))
    {
        return Some(Intent::Thanks);
    }
    let has_number = words.iter().any(|word| word.parse::<f64>().is_ok());
    if !has_number
        && words.iter().any(|word| TEMPERATURE_WORDS.contains(word))
        && !words
            .first()
            .is_some_and(|word| SETTING_VERBS.contains(word))
    {
        let rest: Vec<&str> = words
            .iter()
            .copied()
            .filter(|word| !TEMPERATURE_FILLER.contains(word))
            .collect();
        return Some(Intent::Query {
            asked: Asked::Temperature,
            subject: subject(&rest)?,
        });
    }
    if let ["what" | "what's" | "whats", middle @ .., "set", "to"] = words.as_slice() {
        let rest: Vec<&str> = middle
            .iter()
            .copied()
            .filter(|word| *word != "is")
            .collect();
        return Some(Intent::Query {
            asked: Asked::Status,
            subject: subject(&rest)?,
        });
    }
    if words.first() == Some(&"check") || words.iter().any(|word| STATUS_WORDS[1..].contains(word))
    {
        let rest: Vec<&str> = words
            .iter()
            .copied()
            .filter(|word| !STATUS_FILLER.contains(word))
            .collect();
        return Some(Intent::Query {
            asked: Asked::Status,
            subject: subject(&rest)?,
        });
    }
    match words.as_slice() {
        ["undo" | "revert", rest @ ..] if rest.iter().all(|word| UNDO_WORDS.contains(word)) => {
            Some(Intent::Undo)
        }
        ["put" | "change" | "set", "it" | "that", "back"] => Some(Intent::Undo),
        ["what" | "how", "about", rest @ ..] | ["and", rest @ ..] => Some(Intent::FollowUp {
            subject: subject(rest)?,
        }),
        ["turn" | "switch", "on", rest @ ..] => control(Action::TurnOn, rest),
        ["turn" | "switch", rest @ .., "on"] => control(Action::TurnOn, rest),
        ["turn" | "switch", "off", rest @ ..] => control(Action::TurnOff, rest),
        ["turn" | "switch", rest @ .., "off"] => control(Action::TurnOff, rest),
        ["open", rest @ ..] => control(Action::Open, rest),
        ["close" | "shut", rest @ ..] => control(Action::Close, rest),
        ["lock", rest @ ..] => control(Action::Lock, rest),
        ["unlock", rest @ ..] => control(Action::Unlock, rest),
        [first, ..] if SETTING_VERBS.contains(first) || SETTING_WORDS.contains(first) => {
            setting(&words)
        }
        // "Actually 22" after a setting: the same device, a new value.
        [value] | [value, "percent" | "degrees" | "degree"] if value.parse::<f64>().is_ok() => {
            setting(&words)
        }
        ["is" | "are", rest @ .., last] => {
            let asked = match *last {
                "on" | "off" | "running" => Asked::Power,
                "open" | "closed" => Asked::Opening,
                "locked" | "unlocked" => Asked::Locking,
                _ => return None,
            };
            Some(Intent::Query {
                asked,
                subject: subject(rest)?,
            })
        }
        // "Porch light on", "kitchen light off".
        [rest @ .., "on"] if is_subject_only(rest) => control(Action::TurnOn, rest),
        [rest @ .., "off"] if is_subject_only(rest) => control(Action::TurnOff, rest),
        // "The hallway" alone continues the previous request.
        _ if words.len() <= MAX_FOLLOW_UP_WORDS && is_subject_only(&words) => {
            Some(Intent::FollowUp {
                subject: subject(&words)?,
            })
        }
        _ => None,
    }
}

fn is_subject_only(words: &[&str]) -> bool {
    !words.is_empty()
        && !words
            .iter()
            .any(|word| NOT_A_SUBJECT.contains(word) || ACTION_VERBS.contains(word))
}

/// "It's on I think", "it is not off", "that's wrong": the user disputes what Luna reported.
fn states_a_fact(words: &[&str]) -> bool {
    words
        .first()
        .is_some_and(|word| STATEMENT_STARTS.contains(word))
        && !words.iter().any(|word| ACTION_VERBS.contains(word))
        && words.iter().any(|word| STATE_WORDS.contains(word))
}

/// "Set the brightness to 50%", "brightness 50%", "set it to 26", "dim the porch to 10 percent".
fn setting(words: &[&str]) -> Option<Intent> {
    let (number_at, value) = words
        .iter()
        .enumerate()
        .rev()
        .take(2)
        .find_map(|(index, word)| Some((index, word.parse::<f64>().ok()?)))?;
    let unit = words.get(number_at + 1).copied();
    let mut kind = match unit {
        None => None,
        Some("percent") => Some(Setting::Brightness),
        Some("degrees" | "degree") => Some(Setting::Temperature),
        Some(_) => return None,
    };
    let mut middle = &words[..number_at];
    if middle.first() == Some(&"dim") {
        kind = Some(Setting::Brightness);
    }
    if middle
        .first()
        .is_some_and(|word| SETTING_VERBS.contains(word))
    {
        middle = &middle[1..];
    }
    if middle.last() == Some(&"to") {
        middle = &middle[..middle.len() - 1];
    }
    let mut named = Vec::new();
    for word in middle {
        match *word {
            "brightness" => kind = Some(Setting::Brightness),
            "temperature" => kind = Some(Setting::Temperature),
            "of" | "in" | "for" => {}
            other if ARTICLES.contains(&other) => {}
            other => named.push(other),
        }
    }
    let subject = if named.is_empty() {
        Subject::Pronoun
    } else {
        subject(&named)?
    };
    Some(Intent::Set {
        kind,
        value,
        subject,
    })
}

fn control(action: Action, rest: &[&str]) -> Option<Intent> {
    Some(Intent::Control {
        action,
        subject: subject(rest)?,
    })
}

fn subject(words: &[&str]) -> Option<Subject> {
    let words: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| !ARTICLES.contains(word))
        .collect();
    let meaningful: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| !SOFT.contains(word))
        .collect();
    match meaningful.as_slice() {
        [] => return None,
        [pronoun] if PRONOUNS.contains(pronoun) => return Some(Subject::Pronoun),
        _ => {}
    }
    match words.as_slice() {
        _ if words.iter().any(|word| BROAD.contains(word)) => None,
        _ => Some(Subject::Named(
            words.iter().map(|word| (*word).to_owned()).collect(),
        )),
    }
}

const ORDINALS: &[&str] = &["first", "second", "third", "fourth"];

/// The devices chosen in answer to "Which one?": "the first one", "second", "both", or a name.
pub fn pick(home: &Home, choices: &[String], answer: &str) -> Option<Vec<String>> {
    let words: Vec<String> = tokens(answer)
        .into_iter()
        .filter(|word| !matches!(word.as_str(), "the" | "one" | "number" | "of" | "them"))
        .collect();
    if let [word] = words.as_slice()
        && matches!(word.as_str(), "both" | "all" | "each" | "every")
    {
        return Some(choices.to_vec());
    }
    let index = match words.as_slice() {
        [word] if word == "last" => choices.len().checked_sub(1),
        [word] => ORDINALS
            .iter()
            .position(|ordinal| ordinal == word)
            .or_else(|| word.parse::<usize>().ok()?.checked_sub(1)),
        _ => None,
    };
    if let Some(index) = index {
        return choices.get(index).cloned().map(|id| vec![id]);
    }
    let phrase: Vec<String> = words.iter().map(|word| stem(word)).collect();
    if phrase.is_empty() {
        return None;
    }
    let entities: Vec<&Entity> = choices.iter().filter_map(|id| home.entity(id)).collect();
    let named: Vec<&&Entity> = entities
        .iter()
        .filter(|entity| names_match(entity, &phrase))
        .collect();
    let covering: Vec<&&Entity> = entities
        .iter()
        .filter(|entity| covers(home, entity, &phrase))
        .collect();
    match (named.as_slice(), covering.as_slice()) {
        ([entity], _) | ([], [entity]) => Some(vec![entity.id.clone()]),
        _ => None,
    }
}

/// Domains an intent can apply to.
pub fn domains(intent: &Intent) -> Vec<&'static str> {
    match intent {
        Intent::Control { action, .. } => CONTROL_DOMAINS
            .iter()
            .copied()
            .filter(|domain| actions::supports_domain(*action, domain))
            .collect(),
        Intent::Query { asked, .. } => match asked {
            Asked::Power => POWER_DOMAINS.to_vec(),
            Asked::Opening => OPENING_DOMAINS.to_vec(),
            Asked::Locking => vec!["lock"],
            Asked::Status => STATUS_DOMAINS.to_vec(),
            Asked::Temperature => TEMPERATURE_DOMAINS.to_vec(),
        },
        Intent::Set { kind, .. } => match kind {
            Some(Setting::Brightness) => vec!["light"],
            Some(Setting::Temperature) => vec!["climate"],
            None => vec!["light", "climate"],
        },
        _ => Vec::new(),
    }
}

/// Most devices Luna offers as choices before leaving an unclear request to the model.
const MAX_CHOICES: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Exact(Vec<String>),
    /// A singular phrase that fits a few devices, such as "the AC" in a home with two.
    Choices(Vec<String>),
    Unknown,
}

/// The entities a subject refers to. Only an exact match is acted on.
pub fn resolve(home: &Home, memory: &Memory, subject: &Subject, domains: &[&str]) -> Resolution {
    match subject {
        Subject::Pronoun => {
            let ids: Vec<String> = memory
                .referenced
                .iter()
                .map(|reference| reference.id.clone())
                .collect();
            let valid = !ids.is_empty()
                && ids
                    .iter()
                    .all(|id| home.entity(id).is_some_and(|entity| fits(entity, domains)));
            if valid {
                Resolution::Exact(ids)
            } else {
                Resolution::Unknown
            }
        }
        Subject::Named(words) => resolve_named(home, words, domains),
    }
}

fn resolve_named(home: &Home, words: &[String], domains: &[&str]) -> Resolution {
    let phrase: Vec<String> = words
        .iter()
        .map(|word| stem(word))
        .filter(|word| !SOFT.contains(&word.as_str()) || names_anything(home, word))
        .collect();
    let candidates: Vec<&Entity> = home
        .entities
        .values()
        .filter(|entity| !entity.internal && fits(entity, domains))
        .filter(|entity| covers(home, entity, &phrase))
        .collect();
    // "The AC" is the climate device, not "AC Display light" or "AC Jet mode" switches.
    let kinds: Vec<&String> = phrase.iter().filter(|word| device_word(word)).collect();
    let of_kind: Vec<&Entity> = candidates
        .iter()
        .copied()
        .filter(|entity| kinds.iter().any(|word| is_kind_of(word, entity)))
        .collect();
    let candidates = if kinds.is_empty() || of_kind.is_empty() {
        candidates
    } else {
        of_kind
    };
    let ids = |entities: &[&Entity]| entities.iter().map(|entity| entity.id.clone()).collect();
    match candidates.len() {
        0 => return Resolution::Unknown,
        1 => return Resolution::Exact(ids(&candidates)),
        _ => {}
    }
    // "AC light" is the one device whose own name has every word, "Bacsil AC Display light".
    let complete: Vec<&Entity> = candidates
        .iter()
        .copied()
        .filter(|entity| {
            let names = name_words(entity);
            phrase.iter().all(|word| names.contains(word))
        })
        .collect();
    // One word like "AC" can fit several devices of that kind, so it still asks.
    if phrase.len() > 1 && complete.len() == 1 {
        return Resolution::Exact(ids(&complete));
    }
    // "kitchen light" picks the light named "Kitchen" over "Kitchen Island".
    let named: Vec<&Entity> = candidates
        .iter()
        .copied()
        .filter(|entity| names_match(entity, &phrase))
        .collect();
    if named.len() == 1 {
        return Resolution::Exact(ids(&named));
    }
    // "kitchen lights" means every matching device when they share one area.
    let plural = words
        .iter()
        .any(|word| word.ends_with('s') && device_word(&stem(word)));
    let area = candidates[0].area_id.as_deref();
    let same_area = area.is_some()
        && candidates
            .iter()
            .all(|entity| entity.area_id.as_deref() == area);
    if plural && same_area {
        Resolution::Exact(ids(&candidates))
    } else if !plural && candidates.len() <= MAX_CHOICES {
        Resolution::Choices(ids(&candidates))
    } else {
        Resolution::Unknown
    }
}

/// Every phrase word names the entity, its area, or its kind of device.
fn covers(home: &Home, entity: &Entity, phrase: &[String]) -> bool {
    let mut known: HashSet<String> = name_words(entity);
    if let Some(area) = entity.area_id.as_deref().and_then(|id| home.area(id)) {
        known.extend(words(&area.name));
        known.extend(area.aliases.iter().flat_map(|alias| words(alias)));
    }
    let mut named = false;
    for word in phrase {
        if known.contains(word) {
            named = true;
        } else if !is_kind_of(word, entity) {
            return false;
        }
    }
    // "The AC" names only a kind of device, so every device of that kind fits.
    named || phrase.iter().all(|word| device_word(word))
}

fn names_anything(home: &Home, word: &str) -> bool {
    home.entities
        .values()
        .any(|entity| !entity.internal && name_words(entity).contains(word))
        || home
            .areas
            .iter()
            .any(|area| words(&area.name).contains(word))
}

fn names_match(entity: &Entity, phrase: &[String]) -> bool {
    let wanted: HashSet<&String> = phrase.iter().filter(|word| !device_word(word)).collect();
    if wanted.is_empty() {
        return false;
    }
    std::iter::once(&entity.name)
        .chain(&entity.aliases)
        .any(|name| {
            let words = words(name);
            words
                .iter()
                .filter(|word| !device_word(word))
                .collect::<HashSet<_>>()
                == wanted
        })
}

fn name_words(entity: &Entity) -> HashSet<String> {
    let mut known = words(&entity.name);
    known.extend(entity.aliases.iter().flat_map(|alias| words(alias)));
    known
}

/// A domain, or `sensor.temperature` for sensors that measure temperature.
fn fits(entity: &Entity, domains: &[&str]) -> bool {
    domains.iter().any(|domain| match *domain {
        "sensor.temperature" => {
            entity.domain() == "sensor" && entity.device_class() == Some("temperature")
        }
        domain => domain == entity.domain(),
    })
}

/// "Blinds" are covers that are blinds, not the garage door.
fn is_kind_of(word: &str, entity: &Entity) -> bool {
    let domain = entity.domain();
    let class = entity.device_class();
    let of_domain = DEVICE_WORDS
        .iter()
        .any(|(kind, domains)| *kind == word && domains.contains(&domain));
    of_domain
        && match (word, domain) {
            ("blind" | "shade" | "curtain" | "shutter", "cover") => matches!(
                class,
                None | Some("blind" | "shade" | "curtain" | "shutter" | "awning")
            ),
            ("door", "cover") => matches!(class, Some("door" | "garage" | "gate")),
            ("door", "binary_sensor") => matches!(class, Some("door" | "garage_door" | "opening")),
            ("window", "cover") => matches!(class, None | Some("window")),
            ("window", "binary_sensor") => class == Some("window"),
            _ => true,
        }
}

fn device_word(word: &str) -> bool {
    DEVICE_WORDS.iter().any(|(kind, _)| *kind == word)
}

fn words(text: &str) -> HashSet<String> {
    tokens(text).iter().map(|word| stem(word)).collect()
}

fn stem(word: &str) -> String {
    word.strip_suffix('s')
        .filter(|stem| stem.len() >= 3 && !stem.ends_with('s'))
        .unwrap_or(word)
        .to_owned()
}

/// Lowercase words without punctuation or polite filler at either end.
fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text
        .to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .replace('%', " percent ")
        .replace('\u{b0}', " degrees ")
        .chars()
        .collect();
    let cleaned: String = chars
        .iter()
        .enumerate()
        .map(|(index, &c)| {
            let decimal = c == '.'
                && index > 0
                && chars[index - 1].is_ascii_digit()
                && chars.get(index + 1).is_some_and(char::is_ascii_digit);
            if c.is_alphanumeric() || c == '\'' || decimal {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut words: Vec<String> = cleaned.split_whitespace().map(str::to_owned).collect();
    while words.len() > 1
        && words
            .first()
            .is_some_and(|word| FILLER.contains(&word.as_str()))
    {
        words.remove(0);
    }
    while words.len() > 1
        && words
            .last()
            .is_some_and(|word| TRAILING.contains(&word.as_str()))
    {
        words.pop();
    }
    words
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::assistant::session::Turn;
    use crate::home_assistant::model::fixtures::{home, state};

    fn named(words: &[&str]) -> Subject {
        Subject::Named(words.iter().map(|word| (*word).to_owned()).collect())
    }

    fn resolve_text(home: &Home, request: &str) -> Option<Vec<String>> {
        let intent = parse(request)?;
        let subject = match &intent {
            Intent::Control { subject, .. } | Intent::Query { subject, .. } => subject.clone(),
            _ => return None,
        };
        match resolve(home, &Memory::default(), &subject, &domains(&intent)) {
            Resolution::Exact(ids) => Some(ids),
            _ => None,
        }
    }

    fn porch_home() -> Home {
        let mut home = home();
        home.apply_state(
            "light.front_porch",
            Some(state(
                "light.front_porch",
                "off",
                json!({"friendly_name": "Front Porch"}),
            )),
        );
        home
    }

    #[test]
    fn parses_simple_commands_and_questions() {
        assert_eq!(
            parse("Turn on the porch light."),
            Some(Intent::Control {
                action: Action::TurnOn,
                subject: named(&["porch", "light"])
            })
        );
        assert_eq!(
            parse("Could you switch the kitchen lights off please"),
            Some(Intent::Control {
                action: Action::TurnOff,
                subject: named(&["kitchen", "lights"])
            })
        );
        assert_eq!(
            parse("Is the garage open?"),
            Some(Intent::Query {
                asked: Asked::Opening,
                subject: named(&["garage"])
            })
        );
        assert_eq!(parse("What time is it?"), Some(Intent::Time));
        assert_eq!(
            parse("what\u{2019}s the time right now"),
            Some(Intent::Time)
        );
        assert_eq!(parse("Revert that."), Some(Intent::Undo));
        assert_eq!(parse("oh wait. revert that"), Some(Intent::Undo));
        assert_eq!(parse("The porch is dark. Turn on the light."), None);
        assert_eq!(parse("Are you sure?"), Some(Intent::Recheck));
        for statement in [
            "It is on I thinkn",
            "It is not off.",
            "no it's on",
            "that's wrong",
        ] {
            assert_eq!(parse(statement), Some(Intent::Recheck), "{statement}");
        }
        assert_eq!(
            parse("no, turn it off"),
            Some(Intent::Control {
                action: Action::TurnOff,
                subject: Subject::Pronoun
            })
        );
        assert_eq!(
            parse("Check guest mode status for the router"),
            Some(Intent::Query {
                asked: Asked::Status,
                subject: named(&["guest", "mode", "router"])
            })
        );
        assert_eq!(
            parse("what's the status of the garage"),
            Some(Intent::Query {
                asked: Asked::Status,
                subject: named(&["garage"])
            })
        );
        for request in [
            "sorry turn it back on",
            "oh wait, turn it on again",
            "okay just turn it on for me please",
            "actually turn it back on too",
        ] {
            assert_eq!(
                parse(request),
                Some(Intent::Control {
                    action: Action::TurnOn,
                    subject: Subject::Pronoun
                }),
                "{request}"
            );
        }
        assert_eq!(
            parse("What about the AC?"),
            Some(Intent::FollowUp {
                subject: named(&["ac"])
            })
        );
        assert_eq!(
            parse("and the kitchen?"),
            Some(Intent::FollowUp {
                subject: named(&["kitchen"])
            })
        );
        assert_eq!(parse("what about the kitchen and hallway"), None);
        assert_eq!(
            parse("Sorry. Turn it back on"),
            Some(Intent::Control {
                action: Action::TurnOn,
                subject: Subject::Pronoun
            })
        );
        assert_eq!(
            parse("turn the back porch light off"),
            Some(Intent::Control {
                action: Action::TurnOff,
                subject: named(&["back", "porch", "light"])
            })
        );
        assert_eq!(
            parse("turn it off"),
            Some(Intent::Control {
                action: Action::TurnOff,
                subject: Subject::Pronoun
            })
        );
    }

    #[test]
    fn parses_brightness_and_temperature_settings() {
        let set = |kind, value, subject| {
            Some(Intent::Set {
                kind,
                value,
                subject,
            })
        };
        assert_eq!(
            parse("brightness 50%"),
            set(Some(Setting::Brightness), 50.0, Subject::Pronoun)
        );
        assert_eq!(
            parse("I don\u{2019}t think it changed. Can you set the brightness to 10%"),
            None
        );
        assert_eq!(
            parse("Can you set the brightness to 10%"),
            set(Some(Setting::Brightness), 10.0, Subject::Pronoun)
        );
        assert_eq!(parse("set it to 26"), set(None, 26.0, Subject::Pronoun));
        assert_eq!(
            parse("set it back to 26"),
            set(None, 26.0, Subject::Pronoun)
        );
        assert_eq!(
            parse("make it 24 degrees"),
            set(Some(Setting::Temperature), 24.0, Subject::Pronoun)
        );
        assert_eq!(
            parse("set the back porch to 50%"),
            set(Some(Setting::Brightness), 50.0, named(&["back", "porch"]))
        );
        assert_eq!(
            parse("Set the temperature to 21.5"),
            set(Some(Setting::Temperature), 21.5, Subject::Pronoun)
        );
        assert_eq!(
            parse("set the office ac to 24 degrees"),
            set(Some(Setting::Temperature), 24.0, named(&["office", "ac"]))
        );
        assert_eq!(
            parse("dim the porch light to 30 percent"),
            set(Some(Setting::Brightness), 30.0, named(&["porch", "light"]))
        );
        assert_eq!(parse("Turn it on and set it to 26"), None);
        assert_eq!(parse("set the scene"), None);
    }

    #[test]
    fn complex_requests_go_to_the_model() {
        for request in [
            "It's too bright in the living room.",
            "I'm heading to bed.",
            "Turn off everything downstairs except the hallway light.",
            "Turn off all the lights",
            "Turn on the kitchen and hallway lights",
            "What time does the kitchen light turn off?",
            "Who wrote Hamlet?",
            "Make it brighter",
        ] {
            assert_eq!(parse(request), None, "{request}");
        }
    }

    #[test]
    fn resolves_partial_names_with_a_device_word() {
        assert_eq!(
            resolve_text(&porch_home(), "Turn on the porch light"),
            Some(vec!["light.front_porch".into()])
        );
        assert_eq!(
            resolve_text(&porch_home(), "Is the garage open?"),
            Some(vec!["cover.garage_door".into()])
        );
        assert_eq!(
            resolve_text(&porch_home(), "turn off the living room lamp"),
            Some(vec!["light.living_room_lamp".into()])
        );
    }

    #[test]
    fn ambiguous_or_unknown_names_are_not_guessed() {
        let mut home = porch_home();
        home.apply_state(
            "light.back_porch",
            Some(state(
                "light.back_porch",
                "off",
                json!({"friendly_name": "Back Porch"}),
            )),
        );
        assert_eq!(resolve_text(&home, "Turn on the porch light"), None);
        assert_eq!(resolve_text(&home, "Turn on the attic light"), None);
        assert_eq!(resolve_text(&home, "Turn on the light"), None);
    }

    #[test]
    fn one_device_of_a_kind_is_acted_on_and_a_few_become_choices() {
        let mut home = home();
        let on = domains(&Intent::Control {
            action: Action::TurnOn,
            subject: Subject::Pronoun,
        });
        let ac = named(&["ac"]);
        assert_eq!(
            resolve(&home, &Memory::default(), &ac, &on),
            Resolution::Exact(vec!["climate.thermostat".into()])
        );
        home.apply_state(
            "climate.bedroom_ac",
            Some(state(
                "climate.bedroom_ac",
                "off",
                json!({"friendly_name": "Bedroom AC"}),
            )),
        );
        assert_eq!(
            resolve(&home, &Memory::default(), &ac, &on),
            Resolution::Choices(vec![
                "climate.bedroom_ac".into(),
                "climate.thermostat".into()
            ])
        );
        assert_eq!(
            resolve(&home, &Memory::default(), &named(&["bedroom", "ac"]), &on),
            Resolution::Exact(vec!["climate.bedroom_ac".into()])
        );
    }

    #[test]
    fn exact_names_win_and_plural_area_requests_cover_the_area() {
        let mut home = home();
        home.apply_state(
            "light.kitchen_island",
            Some(state(
                "light.kitchen_island",
                "on",
                json!({"friendly_name": "Kitchen Island"}),
            )),
        );
        assert_eq!(
            resolve_text(&home, "turn off the kitchen light"),
            Some(vec!["light.kitchen".into()])
        );
        assert_eq!(
            resolve_text(&home, "turn off the kitchen island"),
            Some(vec!["light.kitchen_island".into()])
        );
    }

    #[test]
    fn filler_words_are_ignored_unless_a_device_is_named_with_them() {
        let mut home = porch_home();
        assert_eq!(
            resolve_text(&home, "turn the porch light back on"),
            Some(vec!["light.front_porch".into()])
        );
        home.apply_state(
            "light.back_porch",
            Some(state(
                "light.back_porch",
                "off",
                json!({"friendly_name": "Back Porch"}),
            )),
        );
        assert_eq!(
            resolve_text(&home, "turn on the back porch light"),
            Some(vec!["light.back_porch".into()])
        );
    }

    #[test]
    fn a_name_containing_every_word_is_exact() {
        let mut home = home();
        for (id, name) in [
            ("switch.ac_display_light", "Bacsil AC Display light"),
            ("switch.ac_jet_mode", "Bacsil AC Jet mode"),
        ] {
            home.apply_state(id, Some(state(id, "off", json!({"friendly_name": name}))));
        }
        assert_eq!(
            resolve_text(&home, "Is the AC light on?"),
            Some(vec!["switch.ac_display_light".into()])
        );
    }

    #[test]
    fn hidden_entities_are_never_resolved() {
        assert_eq!(resolve_text(&home(), "turn off the hidden relay"), None);
    }

    #[test]
    fn pronouns_use_the_referenced_entities_only_when_they_fit() {
        let home = porch_home();
        let mut memory = Memory::default();
        let mut turn = Turn::default();
        turn.refer("light.front_porch", "on".into());
        memory.record(turn);
        let off = domains(&Intent::Control {
            action: Action::TurnOff,
            subject: Subject::Pronoun,
        });
        assert_eq!(
            resolve(&home, &memory, &Subject::Pronoun, &off),
            Resolution::Exact(vec!["light.front_porch".into()])
        );
        let lock = domains(&Intent::Control {
            action: Action::Lock,
            subject: Subject::Pronoun,
        });
        assert_eq!(
            resolve(&home, &memory, &Subject::Pronoun, &lock),
            Resolution::Unknown
        );
        assert_eq!(
            resolve(&home, &Memory::default(), &Subject::Pronoun, &off),
            Resolution::Unknown
        );
    }
}
