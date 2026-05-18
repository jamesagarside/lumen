//! Local-user authentication and capability-based authorisation.
//!
//! v1 MVP scope per CONTEXT.md §11:
//! - Local users with Argon2id-hashed passwords
//! - Signed session cookies (random token persisted in the topology
//!   DB alongside its owning user id and expiry)
//! - Capability-based authz: roles bundle capabilities, requests are
//!   gated by capability not role
//!
//! Deferred (per CONTEXT.md): OIDC, header-trust mode, custom roles
//! beyond the four defaults, password-reset flow, audit-log UI.
//! Each lands in its own focused PR.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use argon2::password_hash::{rand_core::OsRng as PwOsRng, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rand::RngCore;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

const USERS: TableDefinition<&str, &str> = TableDefinition::new("users");
const SESSIONS: TableDefinition<&str, &str> = TableDefinition::new("sessions");

const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days

/// Capabilities are simple string IDs. We don't use an enum so adding
/// new ones later doesn't require schema-bumping or a feature flag.
pub mod cap {
    pub const VIEW_GRAPH: &str = "view_graph";
    pub const VIEW_DETECTIONS: &str = "view_detections";
    pub const EDIT_DEVICE_LABELS: &str = "edit_device_labels";
    pub const EDIT_DEVICE_POSITIONS: &str = "edit_device_positions";
    pub const INGEST_FLOWS: &str = "ingest_flows";
    pub const INGEST_DETECTIONS: &str = "ingest_detections";
    pub const MANAGE_USERS: &str = "manage_users";
    /// Read + write the admin Settings page (integration credentials,
    /// webhook URLs, etc.). Admin-only by default — secrets are still
    /// encrypted at rest but anyone with this capability can replace
    /// or clear them.
    pub const MANAGE_SETTINGS: &str = "manage_settings";
}

/// Default roles. v1 ships these four; custom roles are deferred.
/// The bundled capability list is the source of truth; UI menus and
/// docs derive from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Operator,
    Viewer,
    NocDisplay,
}

impl Role {
    pub fn capabilities(self) -> &'static [&'static str] {
        match self {
            Role::Admin => &[
                cap::VIEW_GRAPH,
                cap::VIEW_DETECTIONS,
                cap::EDIT_DEVICE_LABELS,
                cap::EDIT_DEVICE_POSITIONS,
                cap::INGEST_FLOWS,
                cap::INGEST_DETECTIONS,
                cap::MANAGE_USERS,
                cap::MANAGE_SETTINGS,
            ],
            Role::Operator => &[
                cap::VIEW_GRAPH,
                cap::VIEW_DETECTIONS,
                cap::EDIT_DEVICE_LABELS,
                cap::EDIT_DEVICE_POSITIONS,
                cap::INGEST_FLOWS,
                cap::INGEST_DETECTIONS,
            ],
            Role::Viewer => &[cap::VIEW_GRAPH, cap::VIEW_DETECTIONS],
            // NOC Display is read-only with chrome stripped via a UI
            // flag — same capability set as Viewer but the UI shell
            // serves the kiosk variant. (Kiosk UI itself lands in a
            // follow-up.)
            Role::NocDisplay => &[cap::VIEW_GRAPH, cap::VIEW_DETECTIONS],
        }
    }

    pub fn has(self, capability: &str) -> bool {
        self.capabilities().contains(&capability)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub role: Role,
    /// Argon2id PHC string. Never returned to API callers.
    pub password_hash: String,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserSummary {
    pub id: String,
    pub email: String,
    pub role: Role,
}

impl From<&User> for UserSummary {
    fn from(u: &User) -> Self {
        Self {
            id: u.id.clone(),
            email: u.email.clone(),
            role: u.role,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    user_id: String,
    expires_at_unix_secs: u64,
}

/// Wraps the redb Database with auth-flavoured operations. Shares
/// the file with TopologyStore — different tables, same DB handle.
#[derive(Clone)]
pub struct AuthStore {
    db: Arc<Database>,
}

impl AuthStore {
    pub fn new(db: Arc<Database>) -> Result<Self> {
        let txn = db.begin_write().context("auth: open write txn")?;
        {
            txn.open_table(USERS).context("auth: open users table")?;
            txn.open_table(SESSIONS)
                .context("auth: open sessions table")?;
        }
        txn.commit().context("auth: commit init")?;
        Ok(Self { db })
    }

    pub fn user_count(&self) -> Result<usize> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(USERS)?;
        Ok(table.len()? as usize)
    }

    /// All users, ordered by `created_at` ascending (oldest first). Used
    /// by the admin Users page and by the "is there at least one admin
    /// left?" guards in `update_role` / `delete_user`.
    pub fn list_users(&self) -> Result<Vec<User>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(USERS)?;
        let mut out: Vec<User> = Vec::new();
        for entry in table.iter()? {
            let (_, v) = entry?;
            out.push(serde_json::from_str(v.value())?);
        }
        out.sort_by_key(|u| u.created_at);
        Ok(out)
    }

    fn admin_count(&self) -> Result<usize> {
        Ok(self
            .list_users()?
            .iter()
            .filter(|u| u.role == Role::Admin)
            .count())
    }

    pub fn create_user(&self, email: &str, password: &str, role: Role) -> Result<User> {
        validate_email(email)?;
        validate_password(password)?;
        // Email uniqueness — keyed scan is fine at admin-list scale.
        if self.find_by_email(email)?.is_some() {
            anyhow::bail!("a user with email {email} already exists");
        }
        let id = random_id();
        let password_hash = hash_password(password)?;
        let user = User {
            id: id.clone(),
            email: email.to_string(),
            role,
            password_hash,
            created_at: SystemTime::now(),
        };
        let serialised = serde_json::to_string(&user).context("auth: serialise user")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(USERS)?;
            table.insert(id.as_str(), serialised.as_str())?;
        }
        txn.commit()?;
        Ok(user)
    }

    /// Set a user's role. Refuses to demote the last remaining admin —
    /// the system must always have at least one admin who can run the
    /// admin panel.
    pub fn update_role(&self, id: &str, role: Role) -> Result<Option<User>> {
        let Some(mut user) = self.find_by_id(id)? else {
            return Ok(None);
        };
        if user.role == role {
            return Ok(Some(user));
        }
        if user.role == Role::Admin && role != Role::Admin && self.admin_count()? <= 1 {
            anyhow::bail!(
                "cannot demote the last admin — promote another user first, then change this one"
            );
        }
        user.role = role;
        self.write_user(&user)?;
        Ok(Some(user))
    }

    /// Set a new password for `id`. Returns false if the user doesn't
    /// exist. Used by the admin "reset password" flow — the user picks
    /// a new one on next sign-in (or the admin shares the temp value
    /// out-of-band; we don't email it from the daemon).
    pub fn update_password(&self, id: &str, new_password: &str) -> Result<bool> {
        validate_password(new_password)?;
        let Some(mut user) = self.find_by_id(id)? else {
            return Ok(false);
        };
        user.password_hash = hash_password(new_password)?;
        self.write_user(&user)?;
        // Invalidate every session for this user — the password just
        // changed, so any open browser tabs need to log back in.
        self.delete_sessions_for(id)?;
        Ok(true)
    }

    /// Remove the user and every active session of theirs. Refuses to
    /// delete the last admin (same reasoning as `update_role`).
    pub fn delete_user(&self, id: &str) -> Result<bool> {
        let Some(user) = self.find_by_id(id)? else {
            return Ok(false);
        };
        if user.role == Role::Admin && self.admin_count()? <= 1 {
            anyhow::bail!(
                "cannot delete the last admin — promote another user first, then delete this one"
            );
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(USERS)?;
            table.remove(id)?;
        }
        txn.commit()?;
        self.delete_sessions_for(id)?;
        Ok(true)
    }

    fn write_user(&self, user: &User) -> Result<()> {
        let serialised = serde_json::to_string(user).context("auth: serialise user")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(USERS)?;
            table.insert(user.id.as_str(), serialised.as_str())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Delete every session belonging to `user_id`. Called when a user
    /// is deleted or their password is rotated.
    fn delete_sessions_for(&self, user_id: &str) -> Result<()> {
        let mut to_remove: Vec<String> = Vec::new();
        {
            let txn = self.db.begin_read()?;
            let table = txn.open_table(SESSIONS)?;
            for entry in table.iter()? {
                let (k, v) = entry?;
                if let Ok(session) = serde_json::from_str::<StoredSession>(v.value()) {
                    if session.user_id == user_id {
                        to_remove.push(k.value().to_string());
                    }
                }
            }
        }
        if to_remove.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SESSIONS)?;
            for token in &to_remove {
                table.remove(token.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn find_by_email(&self, email: &str) -> Result<Option<User>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(USERS)?;
        for entry in table.iter()? {
            let (_, v) = entry?;
            let user: User = serde_json::from_str(v.value())?;
            if user.email == email {
                return Ok(Some(user));
            }
        }
        Ok(None)
    }

    pub fn find_by_id(&self, id: &str) -> Result<Option<User>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(USERS)?;
        Ok(table
            .get(id)?
            .map(|v| serde_json::from_str::<User>(v.value()))
            .transpose()?)
    }

    pub fn create_session(&self, user_id: &str) -> Result<String> {
        let token = random_token();
        let expires_at = SystemTime::now() + SESSION_TTL;
        let session = StoredSession {
            user_id: user_id.to_string(),
            expires_at_unix_secs: expires_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let serialised = serde_json::to_string(&session)?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SESSIONS)?;
            table.insert(token.as_str(), serialised.as_str())?;
        }
        txn.commit()?;
        Ok(token)
    }

    /// Resolve a session token to its user. Returns Ok(None) if the
    /// session is missing or expired (expired sessions are deleted
    /// as a side effect — opportunistic GC).
    pub fn lookup_session(&self, token: &str) -> Result<Option<User>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(SESSIONS)?;
        let Some(value) = table.get(token)? else {
            return Ok(None);
        };
        let session: StoredSession = serde_json::from_str(value.value())?;
        drop(table);
        drop(txn);
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now_secs >= session.expires_at_unix_secs {
            self.delete_session(token).ok();
            return Ok(None);
        }
        self.find_by_id(&session.user_id)
    }

    pub fn delete_session(&self, token: &str) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SESSIONS)?;
            table.remove(token)?;
        }
        txn.commit()?;
        Ok(())
    }
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut PwOsRng);
    let argon = Argon2::default();
    let hash = argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?
        .to_string();
    Ok(hash)
}

/// Crude but sufficient: must contain an `@` and at least one character
/// either side, with no internal whitespace. Real email validation is
/// famously unbounded; we just catch the obvious typos. We don't require
/// a dot in the domain — lab setups like `admin@localhost` or short TLDs
/// (`alice@corp`) are legitimate.
fn validate_email(email: &str) -> Result<()> {
    let trimmed = email.trim();
    if trimmed.is_empty() || trimmed.len() > 320 {
        anyhow::bail!("email must be between 1 and 320 characters");
    }
    if trimmed.contains(char::is_whitespace) {
        anyhow::bail!("email must not contain whitespace");
    }
    let mut parts = trimmed.splitn(2, '@');
    let (local, domain) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if local.is_empty() || domain.is_empty() {
        anyhow::bail!("'{trimmed}' is not a recognisable email address");
    }
    Ok(())
}

/// Minimum 8 chars, no whitespace at start/end. Deliberately not opinionated
/// — admins manage policy out-of-band, the daemon just enforces a floor.
fn validate_password(password: &str) -> Result<()> {
    if password.len() < 8 {
        anyhow::bail!("password must be at least 8 characters");
    }
    if password != password.trim() {
        anyhow::bail!("password must not start or end with whitespace");
    }
    Ok(())
}

fn random_id() -> String {
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_store() -> (tempfile::TempDir, AuthStore) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("topology.redb");
        let db = Arc::new(Database::create(&path).unwrap());
        let store = AuthStore::new(db).unwrap();
        (dir, store)
    }

    #[test]
    fn role_capability_membership() {
        assert!(Role::Admin.has(cap::MANAGE_USERS));
        assert!(!Role::Viewer.has(cap::MANAGE_USERS));
        assert!(Role::Viewer.has(cap::VIEW_GRAPH));
        assert!(Role::NocDisplay.has(cap::VIEW_GRAPH));
        assert!(!Role::NocDisplay.has(cap::EDIT_DEVICE_LABELS));
    }

    #[test]
    fn create_and_find_user() {
        let (_dir, store) = open_store();
        let u = store
            .create_user("alice@example.com", "s3cret!!", Role::Admin)
            .unwrap();
        assert_eq!(store.user_count().unwrap(), 1);
        let found = store
            .find_by_email("alice@example.com")
            .unwrap()
            .expect("must find");
        assert_eq!(found.id, u.id);
        assert!(verify_password("s3cret!!", &found.password_hash));
        assert!(!verify_password("wrong", &found.password_hash));
    }

    #[test]
    fn session_lookup_and_expiry() {
        let (_dir, store) = open_store();
        let u = store
            .create_user("a@b.com", "pw12345!", Role::Operator)
            .unwrap();
        let token = store.create_session(&u.id).unwrap();
        let looked_up = store.lookup_session(&token).unwrap().unwrap();
        assert_eq!(looked_up.id, u.id);

        store.delete_session(&token).unwrap();
        assert!(store.lookup_session(&token).unwrap().is_none());
    }

    #[test]
    fn bad_password_doesnt_verify() {
        let hash = hash_password("correct").unwrap();
        assert!(verify_password("correct", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    // ── User CRUD (admin panel surface) ─────────────────────────────────

    #[test]
    fn list_users_returns_creation_order() {
        let (_dir, store) = open_store();
        let a = store
            .create_user("a@x.com", "passw0rd", Role::Admin)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let b = store
            .create_user("b@x.com", "passw0rd", Role::Viewer)
            .unwrap();
        let list = store.list_users().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, a.id);
        assert_eq!(list[1].id, b.id);
    }

    #[test]
    fn duplicate_email_is_rejected() {
        let (_dir, store) = open_store();
        store
            .create_user("a@x.com", "passw0rd", Role::Admin)
            .unwrap();
        let err = store
            .create_user("a@x.com", "passw0rd", Role::Viewer)
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn update_role_changes_role() {
        let (_dir, store) = open_store();
        store
            .create_user("admin@x.com", "passw0rd", Role::Admin)
            .unwrap();
        let viewer = store
            .create_user("v@x.com", "passw0rd", Role::Viewer)
            .unwrap();
        store.update_role(&viewer.id, Role::Operator).unwrap();
        assert_eq!(
            store.find_by_id(&viewer.id).unwrap().unwrap().role,
            Role::Operator
        );
    }

    #[test]
    fn cannot_demote_last_admin() {
        let (_dir, store) = open_store();
        let admin = store
            .create_user("admin@x.com", "passw0rd", Role::Admin)
            .unwrap();
        let err = store.update_role(&admin.id, Role::Viewer).unwrap_err();
        assert!(err.to_string().contains("last admin"));
        // With a second admin, demotion is fine.
        let other = store
            .create_user("a2@x.com", "passw0rd", Role::Admin)
            .unwrap();
        store.update_role(&admin.id, Role::Viewer).unwrap();
        assert_eq!(
            store.find_by_id(&other.id).unwrap().unwrap().role,
            Role::Admin
        );
    }

    #[test]
    fn cannot_delete_last_admin() {
        let (_dir, store) = open_store();
        let admin = store
            .create_user("admin@x.com", "passw0rd", Role::Admin)
            .unwrap();
        assert!(store
            .delete_user(&admin.id)
            .unwrap_err()
            .to_string()
            .contains("last admin"));
    }

    #[test]
    fn delete_user_invalidates_their_sessions() {
        let (_dir, store) = open_store();
        store
            .create_user("admin@x.com", "passw0rd", Role::Admin)
            .unwrap();
        let v = store
            .create_user("v@x.com", "passw0rd", Role::Viewer)
            .unwrap();
        let token = store.create_session(&v.id).unwrap();
        assert!(store.lookup_session(&token).unwrap().is_some());
        store.delete_user(&v.id).unwrap();
        assert!(store.lookup_session(&token).unwrap().is_none());
    }

    #[test]
    fn update_password_invalidates_existing_sessions() {
        let (_dir, store) = open_store();
        let u = store
            .create_user("a@x.com", "passw0rd", Role::Admin)
            .unwrap();
        let token = store.create_session(&u.id).unwrap();
        store.update_password(&u.id, "newpassword").unwrap();
        // Old session is gone; user can still sign in with the new password.
        assert!(store.lookup_session(&token).unwrap().is_none());
        let refreshed = store.find_by_email("a@x.com").unwrap().unwrap();
        assert!(verify_password("newpassword", &refreshed.password_hash));
        assert!(!verify_password("passw0rd", &refreshed.password_hash));
    }

    #[test]
    fn short_password_rejected_on_create_and_update() {
        let (_dir, store) = open_store();
        assert!(store.create_user("a@x.com", "short", Role::Admin).is_err());
        let u = store
            .create_user("a@x.com", "passw0rd", Role::Admin)
            .unwrap();
        assert!(store.update_password(&u.id, "tiny").is_err());
    }

    #[test]
    fn malformed_email_rejected() {
        let (_dir, store) = open_store();
        assert!(store
            .create_user("no-at-sign", "passw0rd", Role::Admin)
            .is_err());
        assert!(store
            .create_user("@nolocal", "passw0rd", Role::Admin)
            .is_err());
        assert!(store
            .create_user("local@", "passw0rd", Role::Admin)
            .is_err());
        assert!(store
            .create_user("has space@x.com", "passw0rd", Role::Admin)
            .is_err());
        // Lab-style emails without a domain dot stay legal.
        assert!(store
            .create_user("admin@localhost", "passw0rd", Role::Admin)
            .is_ok());
    }
}
