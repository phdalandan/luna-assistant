//! Whether speech without Luna's name, during a conversation, is meant for her: a recognised
//! request, an action or question about "it" or a device or place in this home, a correction
//! of what Luna said, or a mention of Home Assistant.
use super::route::{self, Intent, Subject};
use crate::home_assistant::model::Home;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relevance {
    Request,
    /// "Thanks": answered, then the conversation ends.
    Thanks,
    /// "Never mind": the conversation ends without a reply.
    Closing,
    Unrelated,
}

const CLOSINGS: &[&str] = &[
    "never mind",
    "nevermind",
    "that's all",
    "thats all",
    "that's it",
    "thats it",
    "nothing",
    "forget it",
    "cancel",
    "stop",
    "no thanks",
    "bye",
    "goodbye",
    "i'm good",
    "im good",
    "all good",
];
const AFFIRMATIVE: &[&str] = &[
    "yes",
    "yeah",
    "yea",
    "yep",
    "yup",
    "ya",
    "sure",
    "sure thing",
    "absolutely",
    "definitely",
    "certainly",
    "of course",
    "please",
    "please do",
    "do it",
    "do that",
    "go ahead",
    "go for it",
    "ok",
    "okay",
    "alright",
    "all right",
    "sounds good",
    "why not",
    "correct",
    "confirm",
    "that would be great",
    "that'd be great",
];
const NEGATIVE: &[&str] = &[
    "no",
    "nope",
    "nah",
    "no thanks",
    "no thank you",
    "not now",
    "not yet",
    "not really",
    "no need",
    "don't",
    "dont",
    "do not",
    "don't bother",
    "cancel",
    "stop",
    "never mind",
    "nevermind",
    "leave it",
    "leave them",
];
/// Words that can surround an answer without changing it, as in "Oh, yes please, Luna".
const ANSWER_FILLER: &[&str] = &[
    "please", "luna", "thanks", "thank", "you", "oh", "um", "uh", "well", "just", "then", "hmm",
];

/// "Yes" or "no" in answer to a question from Luna, such as a confirmation or an offer. Only
/// replies made entirely of answer words count, so "sure, turn off the kitchen" is a request.
pub fn answer(text: &str) -> Option<bool> {
    let words = plain_words(text);
    let (mut yes, mut no) = (false, false);
    let mut index = 0;
    while index < words.len() {
        let rest = &words[index..];
        if let Some(length) = longest_match(AFFIRMATIVE, rest) {
            yes = true;
            index += length;
        } else if let Some(length) = longest_match(NEGATIVE, rest) {
            no = true;
            index += length;
        } else if ANSWER_FILLER.contains(&rest[0].as_str()) {
            index += 1;
        } else {
            return None;
        }
    }
    match (yes, no) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// The word count of the longest phrase from `phrases` that `words` starts with.
fn longest_match(phrases: &[&str], words: &[String]) -> Option<usize> {
    phrases
        .iter()
        .map(|phrase| phrase.split(' ').collect::<Vec<_>>())
        .filter(|phrase| {
            words.len() >= phrase.len() && words.iter().zip(phrase).all(|(word, part)| word == part)
        })
        .map(|phrase| phrase.len())
        .max()
}

/// Replies that dispute Luna's last answer, at the start of what was said.
const CORRECTIONS: &[&str] = &[
    "yes you can",
    "yes it is",
    "yes there is",
    "no it isn't",
    "no it's not",
    "you're wrong",
    "you are wrong",
    "that's wrong",
    "that's not right",
    "that's not true",
    "try again",
    "check again",
    "look again",
];
const QUESTION_WORDS: &[&str] = &["what", "what's", "whats", "how", "which"];
/// "Make it brighter" refers to what the conversation is about.
const REFERENCES: &[&str] = &["it", "them", "those"];

pub fn classify(home: &Home, text: &str) -> Relevance {
    let words = route::tokens(text);
    if CLOSINGS.contains(&words.join(" ").as_str()) {
        return Relevance::Closing;
    }
    if corrects_luna(text) || mentions_home_assistant(text) {
        return Relevance::Request;
    }
    match route::parse(text) {
        Some(Intent::Thanks) => Relevance::Thanks,
        Some(Intent::Time | Intent::Undo | Intent::Recheck) => Relevance::Request,
        Some(
            Intent::Control { subject, .. }
            | Intent::Query { subject, .. }
            | Intent::Set { subject, .. }
            | Intent::FollowUp { subject },
        ) => match subject {
            Subject::Pronoun => Relevance::Request,
            Subject::Named(named) if mentions_home(home, &named) => Relevance::Request,
            Subject::Named(_) => Relevance::Unrelated,
        },
        None => {
            let asks = words.iter().any(|word| {
                route::ACTION_VERBS.contains(&word.as_str())
                    || QUESTION_WORDS.contains(&word.as_str())
            });
            let refers = words.iter().any(|word| {
                route::BROAD.contains(&word.as_str()) || REFERENCES.contains(&word.as_str())
            });
            if asks && (refers || mentions_home(home, &words)) {
                Relevance::Request
            } else {
                Relevance::Unrelated
            }
        }
    }
}

/// Every word as spoken, without dropping filler, so "yes you can" keeps its "you can".
fn plain_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('\u{2019}', "'")
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn corrects_luna(text: &str) -> bool {
    let words = plain_words(text);
    CORRECTIONS.iter().any(|correction| {
        let correction: Vec<&str> = correction.split(' ').collect();
        words.len() >= correction.len()
            && words
                .iter()
                .zip(&correction)
                .all(|(word, part)| word == part)
    })
}

fn mentions_home_assistant(text: &str) -> bool {
    let words = plain_words(text);
    words
        .windows(2)
        .any(|pair| pair[0] == "home" && pair[1] == "assistant")
        || words
            .iter()
            .any(|word| word == "entity" || word == "entities")
}

/// A device kind, or a word from an entity, area, or floor name in this home.
fn mentions_home(home: &Home, words: &[String]) -> bool {
    words.iter().map(|word| route::stem(word)).any(|word| {
        // Short names count when they carry a number, like "5G" or "4K".
        (word.len() >= 3 || word.len() == 2 && word.chars().any(|c| c.is_ascii_digit()))
            && (route::device_word(&word)
                || route::names_anything(home, &word)
                || home.floors.iter().any(|floor| {
                    std::iter::once(&floor.name)
                        .chain(&floor.aliases)
                        .any(|name| {
                            route::tokens(name)
                                .iter()
                                .any(|part| route::stem(part) == word)
                        })
                }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home_assistant::model::fixtures::home;

    #[test]
    fn follow_ups_about_the_home_are_requests() {
        let home = home();
        for text in [
            "Make it 50%.",
            "Actually, make it 20%.",
            "Turn it off.",
            "Make it brighter.",
            "How about the kitchen?",
            "Actually, revert that.",
            "Are you sure?",
            "Is the garage open?",
            "Turn everything off downstairs.",
            "And the hallway.",
            "What time is it?",
        ] {
            assert_eq!(classify(&home, text), Relevance::Request, "{text}");
        }
    }

    #[test]
    fn corrections_and_mentions_of_home_assistant_are_for_luna() {
        let home = home();
        for text in [
            "Yes you can. It's an entity in the Home Assistant.",
            "It's an entity in Home Assistant.",
            "No, it's not.",
            "That's wrong.",
            "Check again.",
        ] {
            assert_eq!(classify(&home, text), Relevance::Request, "{text}");
        }
    }

    #[test]
    fn yes_and_no_answer_luna_s_questions() {
        for yes in [
            "Yes.",
            "Yep",
            "Yup!",
            "Sure",
            "Absolutely.",
            "Of course",
            "Yeah, go ahead",
            "Go ahead, please.",
            "Oh yes please, Luna",
            "Sounds good",
        ] {
            assert_eq!(answer(yes), Some(true), "{yes}");
        }
        for no in [
            "No.",
            "Nope",
            "Nah",
            "No thanks",
            "Not now",
            "Don't.",
            "Nope, leave it",
        ] {
            assert_eq!(answer(no), Some(false), "{no}");
        }
        for neither in [
            "Turn off the lights.",
            "Sure, turn off the kitchen",
            "Yes and no",
            "Thank you",
        ] {
            assert_eq!(answer(neither), None, "{neither}");
        }
    }

    #[test]
    fn short_device_names_with_numbers_are_recognised() {
        let mut home = home();
        home.apply_state(
            "switch.guest_wifi_5g",
            Some(crate::home_assistant::model::fixtures::state(
                "switch.guest_wifi_5g",
                "off",
                serde_json::json!({"friendly_name": "Guest WIFI 5G"}),
            )),
        );
        assert_eq!(
            classify(&home, "Can you turn on the 5G?"),
            Relevance::Request
        );
        assert_eq!(classify(&home, "Turn on the 4K."), Relevance::Unrelated);
    }

    #[test]
    fn background_conversation_is_unrelated() {
        let home = home();
        for text in [
            "Did you see the game last night?",
            "I think we should order pizza.",
            "Pass the salt, please.",
            "She said she would call back later.",
            "What's the status of the delivery?",
            "The weather is nice today.",
            "You can sit here.",
            "Yes, I think so.",
        ] {
            assert_eq!(classify(&home, text), Relevance::Unrelated, "{text}");
        }
    }

    #[test]
    fn thanks_and_closings_end_the_conversation() {
        let home = home();
        assert_eq!(classify(&home, "Thanks."), Relevance::Thanks);
        assert_eq!(classify(&home, "Thank you, Luna."), Relevance::Thanks);
        assert_eq!(classify(&home, "Never mind."), Relevance::Closing);
        assert_eq!(classify(&home, "That's all."), Relevance::Closing);
    }
}
