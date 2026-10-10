//! Decides from a transcript whether Luna was spoken to, and extracts the request.
//! The name must be used to address Luna: at the start ("Luna, …", "Hey Luna …"), at the end
//! ("…, Luna?"), or set off by commas in the middle ("Could you, Luna, …"). A mention such as
//! "I told Luna about it" does not count.

/// Transcriptions of "Luna" that only count when the word is used as a name.
const NAMES: &[&str] = &["luna", "loona", "lunar", "luner"];
const GREETINGS: &[&str] = &["hey", "hi", "hello", "ok", "okay", "oh", "yo"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressed {
    /// Luna was addressed with a request, which no longer contains the name. When the name
    /// sits between two sentences, as in "Dinner was great, Luna. Turn off the lights.", either
    /// could be the request, so both are given, the likelier first.
    Request(Vec<String>),
    /// Only the name was said, so Luna should listen for the request.
    NameOnly,
    NotForLuna,
}

pub fn extract(transcript: &str) -> Addressed {
    let sentences = sentences(&clean(transcript));
    for (index, sentence) in sentences.iter().enumerate() {
        let Some((rest, at_end)) = without_name(sentence) else {
            continue;
        };
        if has_words(&rest) {
            let mut candidates = vec![finish(&rest)];
            if let Some(next) = sentences
                .get(index + 1)
                .filter(|next| at_end && has_words(next))
            {
                candidates.push(finish(next));
            }
            return Addressed::Request(candidates);
        }
        // "Luna. Turn off the lights." or "Turn off the lights. Luna?"
        let neighbour = sentences.get(index + 1).or_else(|| {
            index
                .checked_sub(1)
                .and_then(|before| sentences.get(before))
        });
        return match neighbour {
            Some(neighbour) if has_words(neighbour) => Addressed::Request(vec![finish(neighbour)]),
            _ => Addressed::NameOnly,
        };
    }
    Addressed::NotForLuna
}

/// Drops transcription markers such as "[BLANK_AUDIO]" or "(wind blowing)".
fn clean(transcript: &str) -> String {
    let mut text = String::with_capacity(transcript.len());
    let mut depth = 0usize;
    for c in transcript.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => text.push(c),
            _ => {}
        }
    }
    text.trim().to_owned()
}

fn sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            sentences.push(std::mem::take(&mut current));
        }
    }
    sentences.push(current);
    sentences
        .into_iter()
        .map(|sentence| sentence.trim().to_owned())
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

#[derive(Debug)]
struct Word<'a> {
    text: &'a str,
    /// The word is followed by a comma or similar pause.
    pause_after: bool,
}

fn words(sentence: &str) -> Vec<Word<'_>> {
    sentence
        .split_whitespace()
        .map(|raw| {
            let text = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
            let pause_after = raw.ends_with([',', ';', ':', '-']);
            Word { text, pause_after }
        })
        .filter(|word| !word.text.is_empty())
        .collect()
}

fn is_name(word: &Word<'_>) -> bool {
    NAMES.contains(&word.text.to_lowercase().as_str())
}

/// The sentence without the name, if the name is used to address Luna, and whether the name
/// ended the sentence.
fn without_name(sentence: &str) -> Option<(String, bool)> {
    let words = words(sentence);
    let position = words.iter().position(is_name)?;
    let leading_greetings = words[..position]
        .iter()
        .all(|word| GREETINGS.contains(&word.text.to_lowercase().as_str()));
    let at_end = position == words.len() - 1;
    let set_off = position > 0 && words[position - 1].pause_after && words[position].pause_after;
    let kept: Vec<&str> = if leading_greetings {
        words[position + 1..].iter().map(|word| word.text).collect()
    } else if at_end || set_off {
        words
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != position)
            .map(|(_, word)| word.text)
            .collect()
    } else {
        return None;
    };
    Some((kept.join(" "), at_end))
}

fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

fn finish(text: &str) -> String {
    let text = text.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
    crate::actions::capitalize(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> Addressed {
        Addressed::Request(vec![text.into()])
    }

    #[test]
    fn the_name_at_the_start_is_removed() {
        assert_eq!(
            extract("Luna, turn off the lights."),
            request("Turn off the lights")
        );
        assert_eq!(
            extract("Hey Luna, is the garage open?"),
            request("Is the garage open")
        );
        assert_eq!(
            extract("Hey, Luna. Is the garage open?"),
            request("Is the garage open")
        );
    }

    #[test]
    fn the_name_at_the_end_is_removed() {
        assert_eq!(
            extract("Can you turn off the lights, Luna?"),
            request("Can you turn off the lights")
        );
        assert_eq!(
            extract("Turn the bedroom AC down, Luna."),
            request("Turn the bedroom AC down")
        );
        assert_eq!(
            extract("Could you check the temperature in the living room Luna?"),
            request("Could you check the temperature in the living room")
        );
    }

    #[test]
    fn the_name_in_the_middle_counts_when_set_off_by_commas() {
        assert_eq!(
            extract("Could you, Luna, check the temperature in the kitchen?"),
            request("Could you check the temperature in the kitchen")
        );
    }

    #[test]
    fn mentioning_luna_is_not_addressing_her() {
        for transcript in [
            "I saw Luna at the park yesterday.",
            "I told Luna about the party and she laughed.",
            "We watched the lunar eclipse last night.",
            "The tuna sandwich was great.",
            "",
            "[BLANK_AUDIO]",
        ] {
            assert_eq!(extract(transcript), Addressed::NotForLuna, "{transcript}");
        }
    }

    #[test]
    fn only_the_addressed_sentence_is_kept() {
        assert_eq!(
            extract("Dinner was great. Luna, turn off the kitchen lights."),
            request("Turn off the kitchen lights")
        );
    }

    #[test]
    fn a_name_between_sentences_offers_both() {
        assert_eq!(
            extract("Dinner was great, Luna. Turn off the kitchen lights."),
            Addressed::Request(vec![
                "Dinner was great".into(),
                "Turn off the kitchen lights".into()
            ])
        );
    }

    #[test]
    fn the_name_alone_waits_for_the_request() {
        assert_eq!(extract("Luna?"), Addressed::NameOnly);
        assert_eq!(extract("Hey Luna."), Addressed::NameOnly);
        assert_eq!(
            extract("Luna. Turn off the lights."),
            request("Turn off the lights")
        );
        assert_eq!(
            extract("Turn off the lights. Luna."),
            request("Turn off the lights")
        );
    }

    #[test]
    fn mistranscribed_names_count_only_when_addressing() {
        assert_eq!(
            extract("Lunar, turn on the porch light."),
            request("Turn on the porch light")
        );
    }
}
