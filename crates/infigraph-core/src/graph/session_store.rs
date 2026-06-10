use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub id: String,
    pub summary: String,
    #[serde(default)]
    pub pending_tasks: String,
    #[serde(default)]
    pub decisions: String,
    #[serde(default)]
    pub files_touched: String,
    #[serde(default)]
    pub constraints: String,
    #[serde(default)]
    pub assumptions: String,
    #[serde(default)]
    pub blockers: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn open(project_root: &Path) -> Result<Self> {
        let sessions_dir = project_root.join(".infigraph").join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let store = Self { sessions_dir };
        store.migrate_from_kuzu()?;
        Ok(store)
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    pub fn save(&self, session: &SessionData) -> Result<()> {
        let path = self.session_path(&session.id);
        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn load(&self, session_id: &str) -> Result<Option<SessionData>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let session: SessionData = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse session: {}", path.display()))?;
        Ok(Some(session))
    }

    pub fn list_all(&self) -> Result<Vec<SessionData>> {
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("session_") && name_str.ends_with(".json") {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    if let Ok(session) = serde_json::from_str::<SessionData>(&content) {
                        sessions.push(session);
                    }
                }
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        Ok(sessions)
    }

    pub fn list_recent(&self, limit: usize) -> Result<Vec<SessionData>> {
        let mut all = self.list_all()?;
        all.truncate(limit);
        Ok(all)
    }

    pub fn list_by_updated(&self) -> Result<Vec<SessionData>> {
        let mut sessions = self.list_all()?;
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(sessions)
    }

    pub fn delete(&self, session_id: &str) -> Result<()> {
        let path = self.session_path(session_id);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    fn read_kuzu_sessions(db_path: &Path) -> Vec<SessionData> {
        let db = match kuzu::Database::new(db_path, kuzu::SystemConfig::default()) {
            Ok(db) => db,
            Err(_) => return Vec::new(),
        };
        let conn = match kuzu::Connection::new(&db) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let query = "MATCH (s:Session) RETURN s.id, s.summary, s.pending_tasks, s.decisions, \
                     s.files_touched, s.created_at, s.updated_at, s.constraints, s.assumptions, s.blockers";
        let result = match conn.query(query) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut collected = Vec::new();
        for row in result {
            let get = |i: usize| {
                row.get(i)
                    .map(|v| v.to_string())
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string()
            };
            let id = get(0);
            if id.is_empty() {
                continue;
            }
            let created: i64 = get(5).parse().unwrap_or(0);
            let updated: i64 = get(6).parse().unwrap_or(created);
            collected.push(SessionData {
                id,
                summary: get(1),
                pending_tasks: get(2),
                decisions: get(3),
                files_touched: get(4),
                constraints: get(7),
                assumptions: get(8),
                blockers: get(9),
                created_at: created,
                updated_at: updated,
            });
        }
        collected
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.json"))
    }

    pub fn open_dir(sessions_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(sessions_dir)?;
        Ok(Self { sessions_dir: sessions_dir.to_path_buf() })
    }

    fn migrate_from_kuzu(&self) -> Result<()> {
        let db_path = self.sessions_dir.join("db");
        if !db_path.exists() {
            return Ok(());
        }

        let sessions = Self::read_kuzu_sessions(&db_path);

        let mut count = 0u32;
        for session in &sessions {
            let json_path = self.session_path(&session.id);
            if json_path.exists() {
                continue;
            }
            let json = serde_json::to_string_pretty(session)?;
            std::fs::write(&json_path, json)?;
            count += 1;
        }

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(self.sessions_dir.join(".migrated_to_json"));
        let _ = std::fs::remove_file(self.sessions_dir.join("latest_session.json"));
        eprintln!("Migrated {count} session(s) from KuzuDB to JSON files, removed old session DB");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, created_at: i64, updated_at: i64) -> SessionData {
        SessionData {
            id: id.to_string(),
            summary: format!("work on {id}"),
            pending_tasks: String::new(),
            decisions: String::new(),
            files_touched: String::new(),
            constraints: String::new(),
            assumptions: String::new(),
            blockers: String::new(),
            created_at,
            updated_at,
        }
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        let s = make_session("session_2026-06-08", 1000, 2000);
        store.save(&s).unwrap();
        let loaded = store.load("session_2026-06-08").unwrap().unwrap();
        assert_eq!(loaded.id, "session_2026-06-08");
        assert_eq!(loaded.updated_at, 2000);
    }

    #[test]
    fn test_list_all_sorted_by_created() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        store.save(&make_session("session_2026-06-05", 100, 200)).unwrap();
        store.save(&make_session("session_2026-06-07", 300, 400)).unwrap();
        store.save(&make_session("session_2026-06-06", 200, 500)).unwrap();

        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "session_2026-06-07");
        assert_eq!(all[1].id, "session_2026-06-06");
        assert_eq!(all[2].id, "session_2026-06-05");
    }

    #[test]
    fn test_list_by_updated_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        store.save(&make_session("session_2026-06-05", 100, 500)).unwrap();
        store.save(&make_session("session_2026-06-07", 300, 300)).unwrap();
        store.save(&make_session("session_2026-06-06", 200, 400)).unwrap();

        let sorted = store.list_by_updated().unwrap();
        assert_eq!(sorted[0].id, "session_2026-06-05");
        assert_eq!(sorted[1].id, "session_2026-06-06");
        assert_eq!(sorted[2].id, "session_2026-06-07");
    }

    #[test]
    fn test_list_recent_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        store.save(&make_session("session_2026-06-05", 100, 100)).unwrap();
        store.save(&make_session("session_2026-06-06", 200, 200)).unwrap();
        store.save(&make_session("session_2026-06-07", 300, 300)).unwrap();

        let recent = store.list_recent(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "session_2026-06-07");
        assert_eq!(recent[1].id, "session_2026-06-06");
    }

    #[test]
    fn test_delete_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        store.save(&make_session("session_2026-06-08", 100, 100)).unwrap();
        assert!(store.load("session_2026-06-08").unwrap().is_some());
        store.delete("session_2026-06-08").unwrap();
        assert!(store.load("session_2026-06-08").unwrap().is_none());
    }

    #[test]
    fn test_load_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open_dir(dir.path()).unwrap();
        assert!(store.load("session_nope").unwrap().is_none());
    }
}
