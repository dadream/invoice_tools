//! Private tables in the existing user-owned database. Never changes core schema/user_version.
use crate::{Job, Profile};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct Repository(Connection);
impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS concur_browser_profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, account TEXT NOT NULL, payload TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS concur_browser_jobs (id TEXT PRIMARY KEY, scope TEXT NOT NULL, snapshot TEXT NOT NULL, revision INTEGER NOT NULL, payload TEXT NOT NULL, UNIQUE(scope,snapshot));")?;
        Ok(Self(conn))
    }
    pub fn profile(&self) -> Result<Option<Profile>> {
        let text: Option<String> = self
            .0
            .query_row(
                "SELECT payload FROM concur_browser_profiles ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        text.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub fn save_profile(&self, mut profile: Profile) -> Result<Profile> {
        let transaction = self.0.unchecked_transaction()?;
        let previous: Option<String> = transaction
            .query_row(
                "SELECT payload FROM concur_browser_profiles ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            let saved: Profile = serde_json::from_str(&previous)?;
            if saved.mapping_digest() == profile.mapping_digest() {
                transaction.commit()?;
                return Ok(saved);
            }
        }
        transaction.execute(
            "INSERT INTO concur_browser_profiles(origin,account,payload) VALUES (?1,?2,'{}')",
            params![profile.origin, profile.account],
        )?;
        profile.version = transaction.last_insert_rowid() as u64;
        transaction.execute(
            "UPDATE concur_browser_profiles SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&profile)?, profile.version],
        )?;
        transaction.commit()?;
        Ok(profile)
    }
    pub fn has_completed_test_for(&self, profile: &Profile) -> Result<bool> {
        let expected = profile.mapping_digest();
        Ok(self.list()?.iter().any(|job| {
            job.test_mode && job.completed() && job.profile.mapping_digest() == expected
        }))
    }
    pub fn create(&self, job: &Job) -> Result<()> {
        let scope = format!(
            "{}|{}|{}",
            job.profile.origin, job.profile.account, job.test_mode
        );
        // Replanning an already written snapshot must not duplicate remote expenses.
        let snapshot = if job.test_mode {
            job.id.clone()
        } else {
            job.snapshot.batch_id.to_string()
        };
        self.0.execute("INSERT INTO concur_browser_jobs(id,scope,snapshot,revision,payload) VALUES(?1,?2,?3,0,?4)",params![job.id,scope,snapshot,serde_json::to_string(job)?])?;
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Job> {
        let text: String = self.0.query_row(
            "SELECT payload FROM concur_browser_jobs WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&text)?)
    }
    pub fn list(&self) -> Result<Vec<Job>> {
        let mut stmt = self
            .0
            .prepare("SELECT payload FROM concur_browser_jobs ORDER BY rowid DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn update(&self, job: &mut Job) -> Result<()> {
        let previous = job.revision;
        job.revision += 1;
        let count = self.0.execute(
            "UPDATE concur_browser_jobs SET revision=?1,payload=?2 WHERE id=?3 AND revision=?4",
            params![job.revision, serde_json::to_string(job)?, job.id, previous],
        )?;
        ensure!(count == 1, "交付状态已更新，请刷新后继续");
        Ok(())
    }
}
