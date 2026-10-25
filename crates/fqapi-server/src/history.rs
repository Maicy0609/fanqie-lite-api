//! A bounded in-memory record of recent requests, for the admin page.
//!
//! Deliberately not persisted and deliberately not the log file: the access log
//! is a stream (stdout, for operators), this is a short window an operator can
//! look at from a browser. Both carry the path only - never the query string,
//! which can hold a media token.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{json, Value};

/// Enough to see a burst of failures, small enough to stay irrelevant to memory.
const CAPACITY: usize = 200;

#[derive(Debug, Clone)]
pub struct Entry {
    pub at: u64,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub millis: u64,
}

pub struct History {
    entries: Mutex<VecDeque<Entry>>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        History {
            entries: Mutex::new(VecDeque::with_capacity(CAPACITY)),
        }
    }

    pub fn record(&self, method: &str, path: &str, status: u16, millis: u64) {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.len() == CAPACITY {
            entries.pop_front();
        }
        entries.push_back(Entry {
            at,
            method: method.to_string(),
            path: path.to_string(),
            status,
            millis,
        });
    }

    /// Newest first, capped at `limit`.
    pub fn recent(&self, limit: usize) -> Vec<Entry> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        entries.iter().rev().take(limit).cloned().collect()
    }

    pub fn as_json(&self, limit: usize) -> Value {
        Value::Array(
            self.recent(limit)
                .into_iter()
                .map(|entry| {
                    json!({
                        "at": entry.at,
                        "method": entry.method,
                        "path": entry.path,
                        "status": entry.status,
                        "ms": entry.millis,
                    })
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_entries_first() {
        let history = History::new();
        history.record("GET", "/first", 200, 1);
        history.record("GET", "/second", 404, 2);
        let recent = history.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].path, "/second");
        assert_eq!(recent[1].path, "/first");
    }

    #[test]
    fn the_buffer_is_bounded_and_drops_the_oldest() {
        let history = History::new();
        for index in 0..CAPACITY + 5 {
            history.record("GET", &format!("/r{index}"), 200, 0);
        }
        let recent = history.recent(CAPACITY + 10);
        assert_eq!(recent.len(), CAPACITY);
        assert_eq!(recent[0].path, format!("/r{}", CAPACITY + 4));
        assert_eq!(recent[CAPACITY - 1].path, "/r5");
    }

    #[test]
    fn limit_truncates_without_reordering() {
        let history = History::new();
        for index in 0..5 {
            history.record("GET", &format!("/r{index}"), 200, 0);
        }
        let recent = history.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].path, "/r4");
        assert_eq!(recent[1].path, "/r3");
    }
}
