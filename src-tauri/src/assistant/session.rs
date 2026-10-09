use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::AssistantError;
use crate::actions::ControlRequest;
use crate::home_assistant::model::Entity;

const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(120);
/// Follow-ups like "revert that" only refer to recent turns.
const MEMORY_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const MAX_REFERENCED: usize = 10;

/// What the conversation is about, kept in Rust rather than inferred from chat text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Memory {
    /// Entities the last turn was about, with the state Luna reported for each.
    pub referenced: Vec<Reference>,
    /// States from before the last executed action, so it can be undone.
    pub last_action: Vec<Entity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub id: String,
    /// What Luna last told the user. Never treated as the current state.
    pub reported: String,
}

/// What one turn referred to and changed. Empty parts leave the memory as it was.
#[derive(Debug, Default)]
pub struct Turn {
    pub referenced: Vec<Reference>,
    pub previous: Vec<Entity>,
}

impl Turn {
    pub fn refer(&mut self, id: &str, reported: String) {
        self.referenced.retain(|reference| reference.id != id);
        self.referenced.push(Reference {
            id: id.to_owned(),
            reported,
        });
    }
}

impl Memory {
    pub fn record(&mut self, turn: Turn) {
        if !turn.referenced.is_empty() {
            self.referenced = turn.referenced;
            self.referenced.truncate(MAX_REFERENCED);
        }
        if !turn.previous.is_empty() {
            self.last_action = turn.previous;
        }
    }
}

/// Tracks the in-flight request, any actions awaiting confirmation, and conversation memory.
#[derive(Default)]
pub struct Session {
    active: Mutex<Option<CancellationToken>>,
    pending: Mutex<Option<PendingConfirmation>>,
    memory: Mutex<Option<(Memory, Instant)>>,
}

struct PendingConfirmation {
    interaction_id: i64,
    requests: Vec<ControlRequest>,
    created: Instant,
}

impl Session {
    pub fn begin(&self) -> Result<CancellationToken, AssistantError> {
        let mut active = lock(&self.active);
        if active.is_some() {
            return Err(AssistantError::Busy);
        }
        // A new request replaces any unanswered confirmation.
        lock(&self.pending).take();
        let token = CancellationToken::new();
        *active = Some(token.clone());
        Ok(token)
    }

    pub fn finish(&self) {
        lock(&self.active).take();
    }

    pub fn cancel(&self) {
        if let Some(token) = lock(&self.active).as_ref() {
            token.cancel();
        }
    }

    pub fn await_confirmation(&self, interaction_id: i64, requests: Vec<ControlRequest>) {
        *lock(&self.pending) = Some(PendingConfirmation {
            interaction_id,
            requests,
            created: Instant::now(),
        });
    }

    pub fn take_confirmation(
        &self,
        interaction_id: i64,
    ) -> Result<Vec<ControlRequest>, AssistantError> {
        match lock(&self.pending).take() {
            Some(confirmation)
                if confirmation.interaction_id == interaction_id
                    && confirmation.created.elapsed() < CONFIRMATION_TIMEOUT =>
            {
                Ok(confirmation.requests)
            }
            _ => Err(AssistantError::ConfirmationExpired),
        }
    }

    /// The conversation memory, or an empty one if the conversation went quiet.
    pub fn memory(&self) -> Memory {
        match lock(&self.memory).as_ref() {
            Some((memory, updated)) if updated.elapsed() < MEMORY_WINDOW => memory.clone(),
            _ => Memory::default(),
        }
    }

    pub fn remember(&self, memory: Memory) {
        *lock(&self.memory) = Some((memory, Instant::now()));
    }

    pub fn forget(&self) {
        lock(&self.memory).take();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_rejects_parallel_requests_and_expires_confirmations() {
        let session = Session::default();
        let _token = session.begin().unwrap();
        assert_eq!(session.begin().err(), Some(AssistantError::Busy));
        session.finish();

        let request: ControlRequest = serde_json::from_value(
            json!({"action": "unlock", "target": {"entities": ["lock.front_door"]}}),
        )
        .unwrap();
        session.await_confirmation(7, vec![request.clone()]);
        assert_eq!(
            session.take_confirmation(8).err(),
            Some(AssistantError::ConfirmationExpired)
        );
        session.await_confirmation(7, vec![request.clone()]);
        assert_eq!(session.take_confirmation(7), Ok(vec![request]));
        assert!(session.take_confirmation(7).is_err());
    }

    #[test]
    fn turns_without_references_keep_the_previous_context() {
        let mut memory = Memory::default();
        let mut turn = Turn::default();
        turn.refer("light.front_porch", "on".into());
        memory.record(turn);
        memory.record(Turn::default());
        assert_eq!(memory.referenced[0].id, "light.front_porch");
    }

    #[test]
    fn memory_survives_between_requests_and_can_be_cleared() {
        let session = Session::default();
        let mut memory = Memory::default();
        let mut turn = Turn::default();
        turn.refer("light.kitchen", "off".into());
        memory.record(turn);
        session.remember(memory.clone());
        assert_eq!(session.memory(), memory);
        session.forget();
        assert_eq!(session.memory(), Memory::default());
    }
}
