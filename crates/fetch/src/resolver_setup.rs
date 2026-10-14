//! In-memory health of resolvers: counters per resolver id, never persisted
//! and never carrying a name, address or path.

use std::collections::HashMap;
use std::sync::Mutex;

/// Failures in a row after which the hint appears.
const STREAK: u32 = 3;
const HINT: &str = "This resolver failed several times in a row; it may be outdated.";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub successes: u32,
    pub failures: u32,
    /// Failures since the last success.
    pub streak: u32,
}

#[derive(Debug, Default)]
pub struct Health {
    by_id: HashMap<u32, Counts>,
}

impl Health {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, id: u32, ok: bool) {
        let c = self.by_id.entry(id).or_default();
        if ok {
            c.successes = c.successes.saturating_add(1);
            c.streak = 0;
        } else {
            c.failures = c.failures.saturating_add(1);
            c.streak = c.streak.saturating_add(1);
        }
    }

    pub fn counts(&self, id: u32) -> Counts {
        self.by_id.get(&id).copied().unwrap_or_default()
    }

    pub fn hint(&self, id: u32) -> Option<&'static str> {
        (self.counts(id).streak >= STREAK).then_some(HINT)
    }
}

static HEALTH: Mutex<Option<Health>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Health) -> R) -> R {
    let mut g = HEALTH.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Health::new))
}

/// Record one outcome in the process-wide health table.
pub fn record(id: u32, ok: bool) {
    with(|h| h.record(id, ok));
}

/// Hint for a resolver in the process-wide table.
pub fn hint(id: u32) -> Option<&'static str> {
    with(|h| h.hint(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streak_logic() {
        let mut h = Health::new();
        for _ in 0..2 {
            h.record(1, false);
        }
        assert_eq!(h.hint(1), None);
        h.record(1, false);
        assert!(h.hint(1).is_some());
        h.record(1, true);
        assert_eq!(h.hint(1), None);
        assert_eq!(h.counts(1).failures, 3);
        assert_eq!(h.hint(2), None);
    }
}
