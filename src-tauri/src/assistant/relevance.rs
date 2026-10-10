//! Whether speech without Luna's name, during a conversation, is meant for her: a recognised
//! request, or an action or question about "it" or a device or place in this home.
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
const QUESTION_WORDS: &[&str] = &["what", "what's", "whats", "how", "which"];
/// "Make it brighter" refers to what the conversation is about.
const REFERENCES: &[&str] = &["it", "them", "those"];

pub fn classify(home: &Home, text: &str) -> Relevance {
    let words = route::tokens(text);
    if CLOSINGS.contains(&words.join(" ").as_str()) {
        return Relevance::Closing;
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

/// A device kind, or a word from an entity, area, or floor name in this home.
fn mentions_home(home: &Home, words: &[String]) -> bool {
    words.iter().map(|word| route::stem(word)).any(|word| {
        word.len() >= 3
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
    fn background_conversation_is_unrelated() {
        let home = home();
        for text in [
            "Did you see the game last night?",
            "I think we should order pizza.",
            "Pass the salt, please.",
            "She said she would call back later.",
            "What's the status of the delivery?",
            "The weather is nice today.",
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
