//! Whether a transcript addresses Luna by her wake word: at the start, at the end, or followed
//! by a pause mid-sentence ("Could you, Luna, …"). "I told Luna about it" does not count.

const GREETINGS: &[&str] = &["hey", "hi", "hello", "ok", "okay", "oh", "yo"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressed {
    /// Possible requests without the name, likelier first. Not `certain` when the name was
    /// mid-sentence, so only a request clearly about the home counts.
    Request {
        candidates: Vec<String>,
        certain: bool,
    },
    /// Only the name was said, so Luna should listen for the request.
    NameOnly,
    NotForLuna,
}

pub fn extract(transcript: &str, wake_word: &str) -> Addressed {
    let name: Vec<String> = wake_word
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let sentences = sentences(&clean(transcript));
    for (index, sentence) in sentences.iter().enumerate() {
        let Some(placement) = place_name(sentence, &name) else {
            continue;
        };
        let (mut candidates, certain) = match placement {
            Placement::Start { after } => (vec![after], true),
            // "Dinner was great, Luna. Turn off the lights." could mean either sentence.
            Placement::End { before } => {
                let next = sentences.get(index + 1).filter(|next| has_words(next));
                (
                    vec![Some(before), next.cloned()]
                        .into_iter()
                        .flatten()
                        .collect(),
                    true,
                )
            }
            Placement::Middle { before, after } => {
                let whole = format!("{before} {after}");
                (vec![after, whole], false)
            }
        };
        candidates.retain(|candidate| has_words(candidate));
        if !candidates.is_empty() {
            let candidates = candidates
                .iter()
                .map(|candidate| finish(candidate))
                .collect();
            return Addressed::Request {
                candidates,
                certain,
            };
        }
        // "Luna. Turn off the lights." or "Turn off the lights. Luna?"
        let neighbour = sentences.get(index + 1).or_else(|| {
            index
                .checked_sub(1)
                .and_then(|before| sentences.get(before))
        });
        return match neighbour {
            Some(neighbour) if has_words(neighbour) => Addressed::Request {
                candidates: vec![finish(neighbour)],
                certain: true,
            },
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

/// The transcript may misspell a longer name by one letter, as in "Lunar" for "Luna".
fn sounds_like(heard: &str, name: &str) -> bool {
    let heard = heard.to_lowercase();
    heard == name || (name.chars().count() >= 4 && edit_distance(&heard, name) <= 1)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, a) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, b) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(a != *b);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

/// Where the name addresses Luna in a sentence, with the words around it.
enum Placement {
    Start { after: String },
    End { before: String },
    Middle { before: String, after: String },
}

fn place_name(sentence: &str, name: &[String]) -> Option<Placement> {
    let words = words(sentence);
    if name.is_empty() || words.len() < name.len() {
        return None;
    }
    let position = (0..=words.len() - name.len()).find(|&start| {
        name.iter()
            .enumerate()
            .all(|(offset, part)| sounds_like(words[start + offset].text, part))
    })?;
    let end = position + name.len();
    let join = |words: &[Word<'_>]| {
        words
            .iter()
            .map(|word| word.text)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (before, after) = (join(&words[..position]), join(&words[end..]));
    let leading_greetings = words[..position]
        .iter()
        .all(|word| GREETINGS.contains(&word.text.to_lowercase().as_str()));
    if leading_greetings {
        Some(Placement::Start { after })
    } else if end == words.len() {
        Some(Placement::End { before })
    } else if words[end - 1].pause_after {
        Some(Placement::Middle { before, after })
    } else {
        None
    }
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
        Addressed::Request {
            candidates: vec![text.into()],
            certain: true,
        }
    }

    fn unsure(candidates: &[&str]) -> Addressed {
        Addressed::Request {
            candidates: candidates.iter().map(|text| (*text).to_owned()).collect(),
            certain: false,
        }
    }

    #[test]
    fn the_name_at_the_start_is_removed() {
        assert_eq!(
            extract("Luna, turn off the lights.", "Luna"),
            request("Turn off the lights")
        );
        assert_eq!(
            extract("Hey Luna, is the garage open?", "Luna"),
            request("Is the garage open")
        );
        assert_eq!(
            extract("Hey, Luna. Is the garage open?", "Luna"),
            request("Is the garage open")
        );
    }

    #[test]
    fn the_name_at_the_end_is_removed() {
        assert_eq!(
            extract("Can you turn off the lights, Luna?", "Luna"),
            request("Can you turn off the lights")
        );
        assert_eq!(
            extract("Turn the bedroom AC down, Luna.", "Luna"),
            request("Turn the bedroom AC down")
        );
        assert_eq!(
            extract(
                "Could you check the temperature in the living room Luna?",
                "Luna"
            ),
            request("Could you check the temperature in the living room")
        );
    }

    #[test]
    fn the_name_in_the_middle_counts_when_a_pause_follows() {
        assert_eq!(
            extract(
                "Could you, Luna, check the temperature in the kitchen?",
                "Luna"
            ),
            unsure(&[
                "Check the temperature in the kitchen",
                "Could you check the temperature in the kitchen"
            ])
        );
        assert_eq!(
            extract("Dinner was great Luna, turn off the kitchen lights", "Luna"),
            unsure(&[
                "Turn off the kitchen lights",
                "Dinner was great turn off the kitchen lights"
            ])
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
            assert_eq!(
                extract(transcript, "Luna"),
                Addressed::NotForLuna,
                "{transcript}"
            );
        }
    }

    #[test]
    fn only_the_addressed_sentence_is_kept() {
        assert_eq!(
            extract(
                "Dinner was great. Luna, turn off the kitchen lights.",
                "Luna"
            ),
            request("Turn off the kitchen lights")
        );
    }

    #[test]
    fn a_name_between_sentences_offers_both() {
        assert_eq!(
            extract(
                "Dinner was great, Luna. Turn off the kitchen lights.",
                "Luna"
            ),
            Addressed::Request {
                candidates: vec![
                    "Dinner was great".into(),
                    "Turn off the kitchen lights".into()
                ],
                certain: true,
            }
        );
    }

    #[test]
    fn the_name_alone_waits_for_the_request() {
        assert_eq!(extract("Luna?", "Luna"), Addressed::NameOnly);
        assert_eq!(extract("Hey Luna.", "Luna"), Addressed::NameOnly);
        assert_eq!(
            extract("Luna. Turn off the lights.", "Luna"),
            request("Turn off the lights")
        );
        assert_eq!(
            extract("Turn off the lights. Luna.", "Luna"),
            request("Turn off the lights")
        );
    }

    #[test]
    fn mistranscribed_names_count_only_when_addressing() {
        assert_eq!(
            extract("Lunar, turn on the porch light.", "Luna"),
            request("Turn on the porch light")
        );
    }

    #[test]
    fn any_wake_word_works_the_same_way() {
        assert_eq!(
            extract("Jarvis, turn off the lights.", "Jarvis"),
            request("Turn off the lights")
        );
        assert_eq!(
            extract("Turn off the lights, Mary Jane.", "Mary Jane"),
            request("Turn off the lights")
        );
        assert_eq!(
            extract("Hey Jarvis, is the garage open?", "Hey Jarvis"),
            request("Is the garage open")
        );
        assert_eq!(
            extract("Luna, turn off the lights.", "Jarvis"),
            Addressed::NotForLuna
        );
        assert_eq!(
            extract("I asked Jarvis yesterday.", "Jarvis"),
            Addressed::NotForLuna
        );
    }

    #[test]
    fn short_names_must_match_exactly() {
        assert_eq!(
            extract("Kay, turn on the fan.", "Kai"),
            Addressed::NotForLuna
        );
        assert_eq!(
            extract("Kai, turn on the fan.", "Kai"),
            request("Turn on the fan")
        );
    }
}
