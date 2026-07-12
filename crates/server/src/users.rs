//! Durable server-side storage: user accounts, sessions, and ICS
//! subscriptions, all backed by a single SQLite database
//! (`JSCAL_DATA_DIR/jscalendar.db`).
//!
//! Each account carries its own upstream JMAP connection settings
//! (`JmapSettings`), so the app password (verified, never stored in
//! recoverable form) and the JMAP credentials (stored in recoverable form,
//! since the server needs to actually present them to the JMAP server) are
//! deliberately different things with different storage treatment. Display
//! preferences (`DisplayPrefs`) live on the account too, so they follow a
//! user across browsers/devices instead of being pinned to one browser's
//! cookie.

use std::path::PathBuf;
use std::sync::Mutex;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use jmap_client::client::Credentials;
use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

pub const BOOTSTRAP_ADMIN_USERNAME: &str = "admin";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// user's behalf on every connection.
#[derive(Debug, Clone, Default)]
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

/// Display preferences, overriding the server-wide `JSCAL_TIMEZONE`/
/// `JSCAL_TIME_FORMAT`/`JSCAL_HOLIDAYS_REGION` defaults for this account.
/// Each field is `""` for "use the server default"; `holidays_region` also
/// accepts the literal `"none"` for "explicitly no holidays calendar",
/// distinct from "" ("no opinion, use whatever the server has configured").
#[derive(Debug, Clone, Default)]
pub struct DisplayPrefs {
    pub timezone: String,
    pub time_format: String,
    pub holidays_region: String,
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    pub jmap: JmapSettings,
    pub prefs: DisplayPrefs,
}

/// A subscribed read-only calendar sourced from an external `.ics` URL.
#[derive(Debug, Clone)]
pub struct IcsSubscription {
    pub id: String,
    pub name: String,
    pub color: String,
    pub url: String,
}

pub struct UserStore {
    conn: Mutex<Connection>,
}

fn row_to_user(row: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get("id")?,
        username: row.get("username")?,
        password_hash: row.get("password_hash")?,
        role: Role::parse(&row.get::<_, String>("role")?),
        jmap: JmapSettings {
            server_url: row.get("jmap_server_url")?,
            username: row.get("jmap_username")?,
            password: row.get("jmap_password")?,
            token: row.get("jmap_token")?,
        },
        prefs: DisplayPrefs {
            timezone: row.get("pref_timezone")?,
            time_format: row.get("pref_time_format")?,
            holidays_region: row.get("pref_holidays_region")?,
        },
    })
}

const USER_COLUMNS: &str = "id, username, password_hash, role, \
    jmap_server_url, jmap_username, jmap_password, jmap_token, \
    pref_timezone, pref_time_format, pref_holidays_region";

impl UserStore {
    /// Opens (creating if needed) the SQLite database at `path` and
    /// ensures its schema exists. `path`'s parent directory is created if
    /// missing; the file itself is chmod'd `0600` since it holds password
    /// hashes and, for each account's JMAP settings, plaintext credentials.
    pub fn open(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&path).expect("failed to open sqlite database");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("failed to set journal_mode");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("failed to enable foreign_keys");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL,
                jmap_server_url TEXT NOT NULL DEFAULT '',
                jmap_username TEXT NOT NULL DEFAULT '',
                jmap_password TEXT NOT NULL DEFAULT '',
                jmap_token TEXT NOT NULL DEFAULT '',
                pref_timezone TEXT NOT NULL DEFAULT '',
                pref_time_format TEXT NOT NULL DEFAULT '',
                pref_holidays_region TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS sessions (
                token TEXT PRIMARY KEY,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS ics_subscriptions (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                color TEXT NOT NULL,
                url TEXT NOT NULL
            );",
        )
        .expect("failed to create schema");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Self {
            conn: Mutex::new(conn),
        }
    }

    pub fn list_users(&self) -> Vec<User> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!("SELECT {USER_COLUMNS} FROM users"))
            .unwrap();
        stmt.query_map([], row_to_user)
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    }

    pub fn get_user(&self, id: &str) -> Option<User> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
            params![id],
            row_to_user,
        )
        .optional()
        .unwrap()
    }

    pub fn find_by_username(&self, username: &str) -> Option<User> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1 COLLATE NOCASE"),
            params![username],
            row_to_user,
        )
        .optional()
        .unwrap()
    }

    pub fn username_taken(&self, username: &str, excluding_id: Option<&str>) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1 COLLATE NOCASE AND id != ?2)",
            params![username, excluding_id.unwrap_or("")],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            != 0
    }

    pub fn admin_count(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM users WHERE role = 'admin'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize
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
            prefs: DisplayPrefs::default(),
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users (id, username, password_hash, role, \
                jmap_server_url, jmap_username, jmap_password, jmap_token) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                user.id,
                user.username,
                user.password_hash,
                user.role.as_str(),
                user.jmap.server_url,
                user.jmap.username,
                user.jmap.password,
                user.jmap.token,
            ],
        )
        .expect("insert user");
        user
    }

    pub fn update_user<F: FnOnce(&mut User)>(&self, id: &str, f: F) -> Option<User> {
        let conn = self.conn.lock().unwrap();
        let mut user = conn
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
                params![id],
                row_to_user,
            )
            .optional()
            .unwrap()?;
        f(&mut user);
        conn.execute(
            "UPDATE users SET username = ?2, password_hash = ?3, role = ?4, \
                jmap_server_url = ?5, jmap_username = ?6, jmap_password = ?7, jmap_token = ?8, \
                pref_timezone = ?9, pref_time_format = ?10, pref_holidays_region = ?11 \
             WHERE id = ?1",
            params![
                user.id,
                user.username,
                user.password_hash,
                user.role.as_str(),
                user.jmap.server_url,
                user.jmap.username,
                user.jmap.password,
                user.jmap.token,
                user.prefs.timezone,
                user.prefs.time_format,
                user.prefs.holidays_region,
            ],
        )
        .expect("update user");
        Some(user)
    }

    pub fn delete_user(&self, id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM users WHERE id = ?1", params![id])
            .expect("delete user")
            > 0
    }

    pub fn create_session(&self, user_id: String) -> String {
        let token = Uuid::new_v4().to_string();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (token, user_id) VALUES (?1, ?2)",
            params![token, user_id],
        )
        .expect("insert session");
        token
    }

    pub fn user_for_session(&self, token: &str) -> Option<User> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!(
                "SELECT {} FROM users JOIN sessions ON sessions.user_id = users.id \
                 WHERE sessions.token = ?1",
                USER_COLUMNS
                    .split(", ")
                    .map(|c| format!("users.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            params![token],
            row_to_user,
        )
        .optional()
        .unwrap()
    }

    pub fn delete_session(&self, token: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM sessions WHERE token = ?1", params![token]);
    }

    pub fn list_ics_subscriptions(&self, user_id: &str) -> Vec<IcsSubscription> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, color, url FROM ics_subscriptions WHERE user_id = ?1")
            .unwrap();
        stmt.query_map(params![user_id], |row| {
            Ok(IcsSubscription {
                id: row.get(0)?,
                name: row.get(1)?,
                color: row.get(2)?,
                url: row.get(3)?,
            })
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    }

    pub fn get_ics_subscription(&self, user_id: &str, id: &str) -> Option<IcsSubscription> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, color, url FROM ics_subscriptions WHERE user_id = ?1 AND id = ?2",
            params![user_id, id],
            |row| {
                Ok(IcsSubscription {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    color: row.get(2)?,
                    url: row.get(3)?,
                })
            },
        )
        .optional()
        .unwrap()
    }

    pub fn create_ics_subscription(
        &self,
        user_id: &str,
        name: String,
        color: String,
        url: String,
    ) -> IcsSubscription {
        let sub = IcsSubscription {
            id: Uuid::new_v4().to_string(),
            name,
            color,
            url,
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO ics_subscriptions (id, user_id, name, color, url) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![sub.id, user_id, sub.name, sub.color, sub.url],
        )
        .expect("insert ics subscription");
        sub
    }

    pub fn update_ics_subscription(
        &self,
        user_id: &str,
        id: &str,
        name: String,
        color: String,
        url: String,
    ) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE ics_subscriptions SET name = ?3, color = ?4, url = ?5 \
             WHERE user_id = ?1 AND id = ?2",
            params![user_id, id, name, color, url],
        )
        .expect("update ics subscription")
            > 0
    }

    pub fn delete_ics_subscription(&self, user_id: &str, id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM ics_subscriptions WHERE user_id = ?1 AND id = ?2",
            params![user_id, id],
        )
        .expect("delete ics subscription")
            > 0
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
