//! AI chat threads, stored in `chats.json` (owner-only permissions, like the other config files).
//! A chat is deleted automatically 7 days after its last message.

use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::Store;

pub const RETENTION_DAYS: i64 = 7;
const FILE: &str = "chats.json";
const MAX_MESSAGES: usize = 200;
const MAX_CHATS: usize = 200;

/// Serializes read-modify-write cycles on the file.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatThread {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Claude Code session to resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Messages as shown in the UI (role, content, tool calls, errors, notices).
    pub messages: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub message_count: usize,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Drops chats whose last activity is older than the retention period.
fn purge(chats: &mut Vec<ChatThread>, now: i64) -> bool {
    let cutoff = now - RETENTION_DAYS * 24 * 3600 * 1000;
    let before = chats.len();
    chats.retain(|c| c.updated_at >= cutoff);
    chats.len() != before
}

fn load(store: &Store) -> Vec<ChatThread> {
    store.read_file(FILE).unwrap_or_default()
}

pub fn list(store: &Store) -> Result<Vec<ChatSummary>> {
    let _g = LOCK.lock().unwrap();
    let mut chats = load(store);
    if purge(&mut chats, now()) {
        store.write_file(FILE, &chats)?;
    }
    chats.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(chats.iter().map(|c| ChatSummary { id: c.id.clone(), title: c.title.clone(), updated_at: c.updated_at, message_count: c.messages.len() }).collect())
}

pub fn get(store: &Store, id: &str) -> Option<ChatThread> {
    let _g = LOCK.lock().unwrap();
    let mut chats = load(store);
    purge(&mut chats, now());
    chats.into_iter().find(|c| c.id == id)
}

pub fn save(store: &Store, mut chat: ChatThread) -> Result<()> {
    let _g = LOCK.lock().unwrap();
    let mut chats = load(store);
    let t = now();
    chat.updated_at = t;
    if chat.created_at == 0 {
        chat.created_at = t;
    }
    chat.title = chat.title.chars().take(120).collect();
    if chat.messages.len() > MAX_MESSAGES {
        chat.messages.drain(..chat.messages.len() - MAX_MESSAGES);
    }
    match chats.iter_mut().find(|c| c.id == chat.id) {
        Some(c) => *c = chat,
        None => chats.push(chat),
    }
    purge(&mut chats, t);
    chats.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    chats.truncate(MAX_CHATS);
    store.write_file(FILE, &chats)
}

pub fn delete(store: &Store, id: &str) -> Result<()> {
    let _g = LOCK.lock().unwrap();
    let mut chats = load(store);
    chats.retain(|c| c.id != id);
    purge(&mut chats, now());
    store.write_file(FILE, &chats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, updated: i64) -> ChatThread {
        ChatThread { id: id.into(), title: id.into(), created_at: updated, updated_at: updated, session_id: None, messages: vec![] }
    }

    #[test]
    fn purges_after_retention() {
        let day = 24 * 3600 * 1000;
        let now = 100 * day;
        let mut v = vec![chat("fresh", now - day), chat("edge", now - 7 * day), chat("old", now - 7 * day - 1)];
        assert!(purge(&mut v, now));
        assert_eq!(v.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["fresh", "edge"]);
    }

    #[test]
    fn save_list_delete() {
        std::env::set_var("SQLIGHTER_NO_KEYRING", "1");
        let dir = std::env::temp_dir().join(format!("sqlighter-chats-{}", uuid::Uuid::new_v4()));
        let store = Store::open(dir.clone()).unwrap();
        let mut c = chat("a", 0);
        c.messages = (0..250).map(|i| serde_json::json!({"role": "user", "content": i.to_string()})).collect();
        save(&store, c).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        save(&store, chat("b", 0)).unwrap();
        let l = list(&store).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].id, "b"); // most recent first
        assert_eq!(get(&store, "a").unwrap().messages.len(), MAX_MESSAGES);
        // An old chat on disk disappears on the next access.
        let mut all: Vec<ChatThread> = store.read_file(FILE).unwrap();
        all[0].updated_at = 1;
        store.write_file(FILE, &all).unwrap();
        assert_eq!(list(&store).unwrap().len(), 1);
        delete(&store, "a").unwrap();
        assert!(list(&store).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
