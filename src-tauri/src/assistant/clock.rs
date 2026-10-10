//! Reads the current time from a Home Assistant clock entity, never from the computer's clock.
use crate::home_assistant::model::{Entity, Home};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockReading {
    Time(String),
    NoClock,
    Unavailable,
    /// Several clocks show different times.
    Ambiguous,
}

pub fn read(home: &Home) -> ClockReading {
    let clocks: Vec<&Entity> = home.entities.values().filter(|e| is_clock(e)).collect();
    let times: Vec<(u32, u32)> = clocks.iter().filter_map(|e| parse_time(&e.state)).collect();
    match times.first() {
        None if clocks.is_empty() => ClockReading::NoClock,
        None => ClockReading::Unavailable,
        Some(first) if times.iter().all(|time| time == first) => {
            ClockReading::Time(format_time(*first))
        }
        Some(_) => {
            let ids: Vec<&str> = clocks.iter().map(|clock| clock.id.as_str()).collect();
            log::warn!("clock entities disagree: {}", ids.join(", "));
            ClockReading::Ambiguous
        }
    }
}

impl ClockReading {
    pub fn reply(&self) -> String {
        match self {
            Self::Time(time) => format!("It's {time}."),
            Self::NoClock => "I can't get the time from Home Assistant.".into(),
            Self::Unavailable => "The clock in Home Assistant isn't available right now.".into(),
            Self::Ambiguous => {
                "Home Assistant has clocks showing different times. Hide the extra ones in Home Assistant."
                    .into()
            }
        }
    }
}

/// A visible, local wall clock sensor such as Home Assistant's Time & Date `sensor.time`.
fn is_clock(entity: &Entity) -> bool {
    let words = format!("{} {}", entity.id, entity.name).to_lowercase();
    let named = words
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "time" || word == "clock");
    entity.domain() == "sensor"
        && !entity.internal()
        && entity.device_class() != Some("timestamp")
        && named
        && !words.contains("utc")
        && (parse_time(&entity.state).is_some() || !entity.is_available())
}

/// Finds the time of day in states like "17:19", "5:19 PM", "2026-10-09, 17:19", or ISO dates.
fn parse_time(state: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = state
        .split([' ', ',', 'T'])
        .filter(|part| !part.is_empty())
        .collect();
    parts.iter().enumerate().find_map(|(index, part)| {
        let mut fields = part.split(':');
        let hour: u32 = fields.next()?.parse().ok()?;
        let minute_text = fields.next()?;
        let minute: u32 = minute_text.get(..2)?.parse().ok()?;
        let suffix = parts.get(index + 1).map(|next| next.to_ascii_lowercase());
        let hour = match suffix.as_deref() {
            Some("pm") if hour < 12 => hour + 12,
            Some("am") if hour == 12 => 0,
            _ => hour,
        };
        (hour < 24 && minute < 60).then_some((hour, minute))
    })
}

fn format_time((hour, minute): (u32, u32)) -> String {
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    format!("{hour}:{minute:02} {suffix}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::home_assistant::model::fixtures::{home, state};

    fn with(states: &[(&str, &str, serde_json::Value)]) -> Home {
        let mut home = home();
        for (id, value, attributes) in states {
            home.apply_state(id, Some(state(id, value, attributes.clone())));
        }
        home
    }

    #[test]
    fn parses_common_clock_formats() {
        assert_eq!(parse_time("17:19"), Some((17, 19)));
        assert_eq!(parse_time("2026-10-09, 17:19"), Some((17, 19)));
        assert_eq!(parse_time("2026-10-09T07:05:00"), Some((7, 5)));
        assert_eq!(parse_time("5:19 PM"), Some((17, 19)));
        assert_eq!(parse_time("12:01 am"), Some((0, 1)));
        assert_eq!(parse_time("on"), None);
        assert_eq!(parse_time("25:00"), None);
    }

    #[test]
    fn reads_the_time_from_the_clock_entity() {
        let home = with(&[("sensor.time", "17:19", json!({"friendly_name": "Time"}))]);
        assert_eq!(read(&home).reply(), "It's 5:19 PM.");
    }

    #[test]
    fn agreeing_clocks_are_not_ambiguous_and_utc_is_ignored() {
        let home = with(&[
            ("sensor.time", "00:07", json!({"friendly_name": "Time"})),
            (
                "sensor.date_time",
                "2026-10-09, 00:07",
                json!({"friendly_name": "Date & Time"}),
            ),
            (
                "sensor.time_utc",
                "07:07",
                json!({"friendly_name": "Time (UTC)"}),
            ),
        ]);
        assert_eq!(read(&home), ClockReading::Time("12:07 AM".into()));
    }

    #[test]
    fn disagreeing_clocks_need_clarification() {
        let home = with(&[
            ("sensor.time", "17:19", json!({"friendly_name": "Time"})),
            (
                "sensor.alarm_clock",
                "07:00",
                json!({"friendly_name": "Alarm clock"}),
            ),
        ]);
        assert_eq!(read(&home), ClockReading::Ambiguous);
    }

    #[test]
    fn missing_or_unavailable_clocks_are_reported() {
        assert_eq!(read(&home()), ClockReading::NoClock);
        let home = with(&[(
            "sensor.time",
            "unavailable",
            json!({"friendly_name": "Time"}),
        )]);
        assert_eq!(read(&home), ClockReading::Unavailable);
    }

    #[test]
    fn timestamp_sensors_are_not_clocks() {
        let home = with(&[(
            "sensor.last_boot_time",
            "2026-10-09T07:05:00+00:00",
            json!({"device_class": "timestamp"}),
        )]);
        assert_eq!(read(&home), ClockReading::NoClock);
    }
}
