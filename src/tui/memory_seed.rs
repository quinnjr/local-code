// src/tui/memory_seed.rs

use tokio::sync::Mutex;

use daimon::memory::Memory;
use daimon::model::types::Message;

/// An unbounded, in-memory `daimon::memory::Memory` implementor seeded from a
/// `Vec<Message>` at construction. Used wherever this plan needs to preserve
/// (or restore) exact conversation history across an `Agent` rebuild —
/// `/model` switching, `/resume`, and initial session resume at TUI mount.
/// Deliberately not `daimon::memory::SlidingWindowMemory`: that type's
/// default 50-message cap would silently evict the very history a rebuild is
/// trying to preserve, at exactly the moment continuity matters most.
pub struct SeededMemory(Mutex<Vec<Message>>);

impl SeededMemory {
    pub fn new(initial_messages: Vec<Message>) -> Self {
        Self(Mutex::new(initial_messages))
    }
}

impl Memory for SeededMemory {
    async fn add_message(&self, message: &Message) -> daimon::Result<()> {
        self.0.lock().await.push(message.clone());
        Ok(())
    }

    async fn get_messages(&self) -> daimon::Result<Vec<Message>> {
        Ok(self.0.lock().await.clone())
    }

    async fn clear(&self) -> daimon::Result<()> {
        self.0.lock().await.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_with_the_seeded_messages() {
        let memory = SeededMemory::new(vec![Message::user("hi"), Message::assistant("hello")]);
        let messages = memory.get_messages().await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn add_message_appends_without_evicting() {
        let memory = SeededMemory::new(
            (0..100)
                .map(|i| Message::user(format!("msg {i}")))
                .collect(),
        );
        memory.add_message(&Message::user("msg 100")).await.unwrap();
        let messages = memory.get_messages().await.unwrap();
        assert_eq!(messages.len(), 101);
        assert_eq!(messages[0].content.as_deref(), Some("msg 0"));
    }

    #[tokio::test]
    async fn clear_empties_the_history() {
        let memory = SeededMemory::new(vec![Message::user("hi")]);
        memory.clear().await.unwrap();
        assert!(memory.get_messages().await.unwrap().is_empty());
    }
}
