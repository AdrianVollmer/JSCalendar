//! Durable local user accounts.
//!
//! Unlike the rest of this app's state, this genuinely persists to disk
//! (`JSCAL_DATA_DIR/users.json`) — real user management (an admin creating
//! accounts that need to still exist tomorrow) requires surviving a
//! restart, unlike e.g. the in-memory-only ICS-subscription list.
//!
//! Each account carries its own upstream JMAP connection settings
//! (`JmapSettings`), so the app password (verified, never stored in
//! recoverable form) and the JMAP credentials (stored in recoverable form,
//! since the server needs to actually present them to the JMAP server) are
//! deliberately different things with different storage treatment.

use std::path::PathBuf;
use std::sync::RwLock;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use jmap_client::client::Credentials;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const BOOTSTRAP_ADMIN_USERNAME: &str = "admin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn parse(s: &str) -> Self {
        if s == "admin" {
            Role::Admin
        } else {
            Role::User
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
}

/// One user's upstream JMAP connection. Stored in recoverable form (not
/// hashed) since the server has to present it to the JMAP server on the
/// user's behalf on every connection — this is the same trust boundary the
/// app already had when credentials only ever lived in memory, just now
/// also written to `users.json` (mode 0600) so it survives a restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JmapSettings {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub token: String,
}

impl JmapSettings {
    pub fn is_configured(&self) -> bool {
        !self.server_url.is_empty()
            && (!self.token.is_empty() || (!self.username.is_empty() && !self.password.is_empty()))
    }

    pub fn credentials(&self) -> Option<Credentials> {
        if !self.token.is_empty() {
            Some(Credentials::Bearer(self.token.clone()))
        } else if !self.username.is_empty() && !self.password.is_empty() {
            Some(Credentials::Basic {
                username: self.username.clone(),
                password: self.password.clone(),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    #[serde(default)]
    pub jmap: JmapSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    token: String,
    user_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    sessions: Vec<SessionRecord>,
}

pub struct UserStore {
    path: PathBuf,
    data: RwLock<StoreData>,
}

impl UserStore {
    pub fn load(path: PathBuf) -> Self {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            data: RwLock::new(data),
        }
    }

    fn persist(&self, data: &StoreData) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(data) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_err() {
            return;
        }
        if std::fs::rename(&tmp, &self.path).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
    }

    pub fn list_users(&self) -> Vec<User> {
        self.data.read().unwrap().users.clone()
    }

    pub fn get_user(&self, id: &str) -> Option<User> {
        self.data
            .read()
            .unwrap()
            .users
            .iter()
            .find(|u| u.id == id)
            .cloned()
    }

    pub fn find_by_username(&self, username: &str) -> Option<User> {
        self.data
            .read()
            .unwrap()
            .users
            .iter()
            .find(|u| u.username.eq_ignore_ascii_case(username))
            .cloned()
    }

    pub fn username_taken(&self, username: &str, excluding_id: Option<&str>) -> bool {
        self.data.read().unwrap().users.iter().any(|u| {
            u.username.eq_ignore_ascii_case(username) && excluding_id != Some(u.id.as_str())
        })
    }

    pub fn admin_count(&self) -> usize {
        self.data
            .read()
            .unwrap()
            .users
            .iter()
            .filter(|u| u.role == Role::Admin)
            .count()
    }

    pub fn create_user(
        &self,
        username: String,
        password_hash: String,
        role: Role,
        jmap: JmapSettings,
    ) -> User {
        let user = User {
            id: Uuid::new_v4().to_string(),
            username,
            password_hash,
            role,
            jmap,
        };
        let mut guard = self.data.write().unwrap();
        guard.users.push(user.clone());
        self.persist(&guard);
        user
    }

    pub fn update_user<F: FnOnce(&mut User)>(&self, id: &str, f: F) -> Option<User> {
        let mut guard = self.data.write().unwrap();
        let user = guard.users.iter_mut().find(|u| u.id == id)?;
        f(user);
        let updated = user.clone();
        self.persist(&guard);
        Some(updated)
    }

    pub fn delete_user(&self, id: &str) -> bool {
        let mut guard = self.data.write().unwrap();
        let before = guard.users.len();
        guard.users.retain(|u| u.id != id);
        guard.sessions.retain(|s| s.user_id != id);
        let changed = guard.users.len() != before;
        if changed {
            self.persist(&guard);
        }
        changed
    }

    pub fn create_session(&self, user_id: String) -> String {
        let token = Uuid::new_v4().to_string();
        let mut guard = self.data.write().unwrap();
        guard.sessions.push(SessionRecord {
            token: token.clone(),
            user_id,
        });
        self.persist(&guard);
        token
    }

    pub fn user_for_session(&self, token: &str) -> Option<User> {
        let guard = self.data.read().unwrap();
        let rec = guard.sessions.iter().find(|s| s.token == token)?;
        guard.users.iter().find(|u| u.id == rec.user_id).cloned()
    }

    pub fn delete_session(&self, token: &str) {
        let mut guard = self.data.write().unwrap();
        let before = guard.sessions.len();
        guard.sessions.retain(|s| s.token != token);
        if guard.sessions.len() != before {
            self.persist(&guard);
        }
    }
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing failed")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn generate_random_password() -> String {
    let mut bytes = [0u8; 18];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Ensures the built-in `admin` account exists, creating it on first
/// startup (seeding its JMAP connection from `JSCAL_DEMO_*`, if set, so
/// `make demo` keeps working end-to-end) and, when `JSCAL_ADMIN_PASSWORD`
/// (or `_FILE`) is set, resetting its password to that value every startup
/// — the "emergency recovery" path: set the env var, restart, log in,
/// change the password from the UI, then unset it again.
pub fn ensure_bootstrap_admin(store: &UserStore) {
    let override_password = crate::state::read_secret("JSCAL_ADMIN_PASSWORD");
    match store.find_by_username(BOOTSTRAP_ADMIN_USERNAME) {
        Some(existing) => {
            if let Some(password) = override_password {
                let hash = hash_password(&password);
                store.update_user(&existing.id, |u| u.password_hash = hash);
                tracing::warn!(
                    "JSCAL_ADMIN_PASSWORD is set: the '{BOOTSTRAP_ADMIN_USERNAME}' \
                     account's password was reset to it on startup"
                );
            }
        }
        None => {
            let password = override_password.unwrap_or_else(|| {
                let generated = generate_random_password();
                tracing::warn!(
                    "no admin account existed yet; created '{BOOTSTRAP_ADMIN_USERNAME}' \
                     with a random password: {generated} \
                     (sign in and change it, or set JSCAL_ADMIN_PASSWORD to control it)"
                );
                generated
            });
            let hash = hash_password(&password);
            let jmap = JmapSettings {
                server_url: std::env::var("JSCAL_DEMO_SERVER_URL").unwrap_or_default(),
                username: std::env::var("JSCAL_DEMO_USERNAME").unwrap_or_default(),
                password: std::env::var("JSCAL_DEMO_PASSWORD").unwrap_or_default(),
                token: String::new(),
            };
            store.create_user(
                BOOTSTRAP_ADMIN_USERNAME.to_string(),
                hash,
                Role::Admin,
                jmap,
            );
        }
    }
}
