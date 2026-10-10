//! The voice conversation state machine. It holds no audio or text, only where the
//! conversation is and when the current state expires, so every transition can be tested.
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// Audio kept from before the wake word.
    pub preroll: Duration,
    /// How long after a reply follow-ups are accepted without the wake word.
    pub active_window: Duration,
    /// The conversation ends after this long without speech.
    pub silence_timeout: Duration,
    /// How long to look for speech after the wake word before treating it as a false alarm.
    pub wake_check: Duration,
    /// The longest single request.
    pub max_utterance: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            preroll: Duration::from_secs(5),
            active_window: Duration::from_secs(20),
            silence_timeout: Duration::from_secs(8),
            wake_check: Duration::from_millis(1500),
            max_utterance: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Only the wake word detector runs.
    Passive,
    /// The wake word fired; checking that someone is actually speaking.
    WakeDetected,
    CapturingCommand,
    /// Transcribing, interpreting, or executing. New speech is ignored.
    Processing,
    Responding,
    /// Listening for a follow-up without the wake word, until the conversation expires.
    AwaitingFollowUp,
}

/// What a finished utterance turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// A request for Luna, or her name alone. A reply follows.
    Request,
    /// The user ended the conversation, as in "never mind".
    Closing,
    /// Not addressed to Luna, or unrelated speech during a conversation.
    Ignored,
}

/// A timer that ran out, so the driver can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expired {
    /// No speech followed the wake word.
    FalseWake,
    /// The request ran too long and must be cut off now.
    Utterance,
    /// The conversation went quiet and returned to passive listening.
    Conversation,
}

pub struct Conversation {
    timing: Timing,
    state: State,
    deadline: Option<Instant>,
    /// Follow-ups are accepted until this time, however much unrelated speech there is.
    window_ends: Option<Instant>,
    /// The current capture began without the wake word.
    follow_up: bool,
    end_after_reply: bool,
}

impl Conversation {
    pub fn new(timing: Timing) -> Self {
        Self {
            timing,
            state: State::Passive,
            deadline: None,
            window_ends: None,
            follow_up: false,
            end_after_reply: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Whether the utterance being handled was spoken without the wake word.
    pub fn is_follow_up(&self) -> bool {
        self.follow_up
    }

    /// The wake word only counts while passive, so one utterance never starts two requests.
    pub fn wake(&mut self, now: Instant) -> bool {
        if self.state != State::Passive {
            return false;
        }
        self.follow_up = false;
        self.enter(State::WakeDetected, Some(now + self.timing.wake_check));
        true
    }

    pub fn speech_started(&mut self, now: Instant) -> bool {
        match self.state {
            State::WakeDetected => self.follow_up = false,
            State::AwaitingFollowUp => self.follow_up = true,
            _ => return false,
        }
        self.enter(
            State::CapturingCommand,
            Some(now + self.timing.max_utterance),
        );
        true
    }

    pub fn utterance_ended(&mut self) -> bool {
        if self.state != State::CapturingCommand {
            return false;
        }
        self.enter(State::Processing, None);
        true
    }

    /// Returns whether the assistant should handle the utterance.
    pub fn heard(&mut self, heard: Heard, now: Instant) -> bool {
        if self.state != State::Processing {
            return false;
        }
        match heard {
            Heard::Request => true,
            Heard::Closing => {
                self.end();
                false
            }
            Heard::Ignored if self.follow_up => {
                self.listen_for_follow_up(now);
                false
            }
            Heard::Ignored => {
                self.end();
                false
            }
        }
    }

    pub fn replying(&mut self, ends_conversation: bool) -> bool {
        if self.state != State::Processing {
            return false;
        }
        self.end_after_reply = ends_conversation;
        self.enter(State::Responding, None);
        true
    }

    pub fn finished_speaking(&mut self, now: Instant) {
        if self.state != State::Responding {
            return;
        }
        if self.end_after_reply {
            self.end();
            return;
        }
        self.window_ends = Some(now + self.timing.active_window);
        self.listen_for_follow_up(now);
    }

    pub fn tick(&mut self, now: Instant) -> Option<Expired> {
        if self.deadline.is_none_or(|deadline| now < deadline) {
            return None;
        }
        match self.state {
            State::WakeDetected => {
                self.end();
                Some(Expired::FalseWake)
            }
            State::CapturingCommand => {
                self.enter(State::Processing, None);
                Some(Expired::Utterance)
            }
            State::AwaitingFollowUp => {
                self.end();
                Some(Expired::Conversation)
            }
            _ => None,
        }
    }

    /// Returns to passive listening and forgets the conversation window.
    pub fn end(&mut self) {
        self.window_ends = None;
        self.follow_up = false;
        self.end_after_reply = false;
        self.enter(State::Passive, None);
    }

    fn listen_for_follow_up(&mut self, now: Instant) {
        let silence = now + self.timing.silence_timeout;
        let deadline = self.window_ends.map_or(silence, |ends| ends.min(silence));
        if deadline <= now {
            self.end();
            return;
        }
        self.enter(State::AwaitingFollowUp, Some(deadline));
    }

    fn enter(&mut self, state: State, deadline: Option<Instant>) {
        self.state = state;
        self.deadline = deadline;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: f32) -> Duration {
        Duration::from_secs_f32(value)
    }

    /// Wake word, request, reply, and spoken reply finished at `start + 2s`.
    fn after_first_reply(start: Instant) -> Conversation {
        let mut conversation = Conversation::new(Timing::default());
        assert!(conversation.wake(start));
        assert!(conversation.speech_started(start));
        assert!(conversation.utterance_ended());
        assert!(conversation.heard(Heard::Request, start + seconds(1.0)));
        assert!(conversation.replying(false));
        conversation.finished_speaking(start + seconds(2.0));
        assert_eq!(conversation.state(), State::AwaitingFollowUp);
        conversation
    }

    #[test]
    fn a_wake_without_speech_returns_to_passive() {
        let start = Instant::now();
        let mut conversation = Conversation::new(Timing::default());
        conversation.wake(start);
        assert_eq!(conversation.tick(start + seconds(1.0)), None);
        assert_eq!(
            conversation.tick(start + seconds(1.6)),
            Some(Expired::FalseWake)
        );
        assert_eq!(conversation.state(), State::Passive);
    }

    #[test]
    fn follow_ups_need_no_wake_word() {
        let start = Instant::now();
        let mut conversation = after_first_reply(start);
        assert!(conversation.speech_started(start + seconds(4.0)));
        assert!(conversation.is_follow_up());
        assert!(conversation.utterance_ended());
        assert!(conversation.heard(Heard::Request, start + seconds(5.0)));
    }

    #[test]
    fn silence_ends_the_conversation() {
        let start = Instant::now();
        let mut conversation = after_first_reply(start);
        assert_eq!(conversation.tick(start + seconds(9.9)), None);
        assert_eq!(
            conversation.tick(start + seconds(10.0)),
            Some(Expired::Conversation)
        );
        assert_eq!(conversation.state(), State::Passive);
    }

    #[test]
    fn unrelated_speech_never_extends_the_conversation() {
        let start = Instant::now();
        let mut conversation = after_first_reply(start);
        let mut now = start + seconds(2.0);
        while conversation.state() == State::AwaitingFollowUp {
            now += seconds(5.0);
            if conversation.tick(now).is_some() {
                break;
            }
            conversation.speech_started(now);
            conversation.utterance_ended();
            assert!(!conversation.heard(Heard::Ignored, now + seconds(1.0)));
        }
        assert_eq!(conversation.state(), State::Passive);
        assert!(now <= start + seconds(2.0 + 20.0 + 5.0));
    }

    #[test]
    fn unaddressed_speech_after_a_false_wake_is_dropped() {
        let start = Instant::now();
        let mut conversation = Conversation::new(Timing::default());
        conversation.wake(start);
        conversation.speech_started(start);
        conversation.utterance_ended();
        assert!(!conversation.heard(Heard::Ignored, start + seconds(1.0)));
        assert_eq!(conversation.state(), State::Passive);
    }

    #[test]
    fn a_request_is_handled_once() {
        let start = Instant::now();
        let mut conversation = Conversation::new(Timing::default());
        conversation.wake(start);
        conversation.speech_started(start);
        conversation.utterance_ended();
        assert!(!conversation.wake(start), "the wake word fired again");
        assert!(!conversation.speech_started(start));
        assert!(!conversation.utterance_ended());
        assert!(conversation.heard(Heard::Request, start));
        assert!(conversation.replying(false));
        assert!(!conversation.replying(false));
    }

    #[test]
    fn long_requests_are_cut_off() {
        let start = Instant::now();
        let mut conversation = Conversation::new(Timing::default());
        conversation.wake(start);
        conversation.speech_started(start);
        assert_eq!(
            conversation.tick(start + seconds(15.0)),
            Some(Expired::Utterance)
        );
        assert_eq!(conversation.state(), State::Processing);
    }

    #[test]
    fn closing_and_thanks_end_the_conversation() {
        let start = Instant::now();
        let mut conversation = after_first_reply(start);
        conversation.speech_started(start + seconds(3.0));
        conversation.utterance_ended();
        assert!(!conversation.heard(Heard::Closing, start + seconds(4.0)));
        assert_eq!(conversation.state(), State::Passive);

        let mut conversation = after_first_reply(start);
        conversation.speech_started(start + seconds(3.0));
        conversation.utterance_ended();
        conversation.heard(Heard::Request, start + seconds(4.0));
        conversation.replying(true);
        conversation.finished_speaking(start + seconds(5.0));
        assert_eq!(conversation.state(), State::Passive);
    }

    #[test]
    fn ending_mid_request_returns_to_passive() {
        let start = Instant::now();
        let mut conversation = after_first_reply(start);
        conversation.speech_started(start + seconds(3.0));
        conversation.utterance_ended();
        conversation.end();
        assert_eq!(conversation.state(), State::Passive);
        assert_eq!(conversation.deadline(), None);
        assert!(!conversation.replying(false));
    }
}
