use std::collections::BTreeMap;

use base64::Engine;
use serde_json::{json, Value};
use url::Url;

use crate::error::Error;
use crate::jscalendar::{Calendar, CalendarEvent, Id};
use crate::protocol::{
    MethodCall, Request, Response, Session, CAPABILITY_CALENDARS, CAPABILITY_CORE,
};

#[derive(Debug, Clone)]
pub enum Credentials {
    Basic { username: String, password: String },
    Bearer(String),
}

impl Credentials {
    fn header_value(&self) -> String {
        match self {
            Credentials::Basic { username, password } => {
                let raw = format!("{username}:{password}");
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(raw)
                )
            }
            Credentials::Bearer(token) => format!("Bearer {token}"),
        }
    }
}

/// A thin, spec-following JMAP client scoped to Core (RFC 8620) + Calendars
/// (RFC 9670) operations.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    session_url: Url,
    creds: Credentials,
    session: Option<Session>,
}

impl Client {
    pub fn new(session_url: Url, creds: Credentials) -> Self {
        Self {
            http: reqwest::Client::new(),
            session_url,
            creds,
            session: None,
        }
    }

    /// Resolve `.well-known/jmap` (or whatever URL was given) and cache the
    /// session object. Must be called before any other operation.
    pub async fn connect(&mut self) -> Result<&Session, Error> {
        let resp = self
            .http
            .get(self.session_url.clone())
            .header("Authorization", self.creds.header_value())
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }
        let session: Session = resp.error_for_status()?.json().await?;
        self.session = Some(session);
        Ok(self.session.as_ref().unwrap())
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    fn api_url(&self) -> Result<&str, Error> {
        self.session
            .as_ref()
            .map(|s| s.api_url.as_str())
            .ok_or_else(|| Error::Protocol("not connected".into()))
    }

    pub fn calendars_account_id(&self) -> Result<String, Error> {
        self.session
            .as_ref()
            .and_then(|s| s.calendars_account_id())
            .map(|s| s.to_string())
            .ok_or(Error::UnsupportedAccount("calendars"))
    }

    /// Issue a raw JMAP request: the low-level primitive everything else is
    /// built on, exposed so callers can batch/chain calls themselves.
    pub async fn call(&self, using: Vec<String>, calls: Vec<MethodCall>) -> Result<Response, Error> {
        let req = Request {
            using,
            method_calls: calls,
        };
        let resp = self
            .http
            .post(self.api_url()?)
            .header("Authorization", self.creds.header_value())
            .json(&req)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }
        let body: Response = resp.error_for_status()?.json().await?;
        Ok(body)
    }

    fn using(&self) -> Vec<String> {
        vec![CAPABILITY_CORE.to_string(), CAPABILITY_CALENDARS.to_string()]
    }

    // ---- Calendars ----------------------------------------------------

    pub async fn get_calendars(&self, account_id: &str) -> Result<Vec<Calendar>, Error> {
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "Calendar/get".into(),
                    json!({ "accountId": account_id, "ids": null }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        let list = result
            .get("list")
            .ok_or_else(|| Error::Protocol("missing list".into()))?;
        Ok(serde_json::from_value(list.clone())?)
    }

    pub async fn create_calendar(&self, account_id: &str, calendar: &Calendar) -> Result<Calendar, Error> {
        let mut create = BTreeMap::new();
        create.insert("new".to_string(), serde_json::to_value(calendar)?);
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "Calendar/set".into(),
                    json!({ "accountId": account_id, "create": create }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        if let Some(err) = result.get("notCreated").and_then(|v| v.get("new")) {
            return Err(Error::Protocol(format!("calendar not created: {err}")));
        }
        let created = result
            .get("created")
            .and_then(|c| c.get("new"))
            .ok_or_else(|| Error::Protocol("missing created calendar".into()))?;
        let mut merged = serde_json::to_value(calendar)?;
        merge_json(&mut merged, created);
        Ok(serde_json::from_value(merged)?)
    }

    pub async fn update_calendar(
        &self,
        account_id: &str,
        id: &str,
        patch: Value,
    ) -> Result<(), Error> {
        let mut update = BTreeMap::new();
        update.insert(id.to_string(), patch);
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "Calendar/set".into(),
                    json!({ "accountId": account_id, "update": update }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        if let Some(err) = result.get("notUpdated").and_then(|v| v.get(id)) {
            return Err(Error::Protocol(format!("calendar not updated: {err}")));
        }
        Ok(())
    }

    pub async fn destroy_calendar(&self, account_id: &str, id: &str) -> Result<(), Error> {
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "Calendar/set".into(),
                    json!({ "accountId": account_id, "destroy": [id] }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        if let Some(err) = result
            .get("notDestroyed")
            .and_then(|v| v.get(id))
        {
            return Err(Error::Protocol(format!("calendar not destroyed: {err}")));
        }
        Ok(())
    }

    // ---- Calendar events ------------------------------------------------

    /// Query events whose time range overlaps `[after, before)` (RFC 9670
    /// §4.3 CalendarEvent/query filter), then fetch the full objects in the
    /// same round trip via a JMAP back-reference.
    pub async fn query_events(
        &self,
        account_id: &str,
        calendar_id: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
    ) -> Result<Vec<CalendarEvent>, Error> {
        let mut filter = serde_json::Map::new();
        if let Some(cid) = calendar_id {
            filter.insert("inCalendars".into(), json!([cid]));
        }
        if let Some(a) = after {
            filter.insert("after".into(), json!(a));
        }
        if let Some(b) = before {
            filter.insert("before".into(), json!(b));
        }
        let query_args = if filter.is_empty() {
            json!({ "accountId": account_id })
        } else {
            json!({ "accountId": account_id, "filter": filter })
        };

        let resp = self
            .call(
                self.using(),
                vec![
                    MethodCall("CalendarEvent/query".into(), query_args, "q0".into()),
                    MethodCall(
                        "CalendarEvent/get".into(),
                        json!({
                            "accountId": account_id,
                            "#ids": {
                                "resultOf": "q0",
                                "name": "CalendarEvent/query",
                                "path": "/ids",
                            },
                        }),
                        "g0".into(),
                    ),
                ],
            )
            .await?;
        let result = resp.result_for("g0")?;
        let list = result
            .get("list")
            .ok_or_else(|| Error::Protocol("missing list".into()))?;
        Ok(serde_json::from_value(list.clone())?)
    }

    pub async fn get_events(&self, account_id: &str, ids: &[Id]) -> Result<Vec<CalendarEvent>, Error> {
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "CalendarEvent/get".into(),
                    json!({ "accountId": account_id, "ids": ids }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        let list = result
            .get("list")
            .ok_or_else(|| Error::Protocol("missing list".into()))?;
        Ok(serde_json::from_value(list.clone())?)
    }

    pub async fn create_event(&self, account_id: &str, event: &CalendarEvent) -> Result<CalendarEvent, Error> {
        let mut create = BTreeMap::new();
        create.insert("new".to_string(), serde_json::to_value(event)?);
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "CalendarEvent/set".into(),
                    json!({ "accountId": account_id, "create": create }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        if let Some(err) = result.get("notCreated").and_then(|v| v.get("new")) {
            return Err(Error::Protocol(format!("event not created: {err}")));
        }
        let created = result
            .get("created")
            .and_then(|c| c.get("new"))
            .ok_or_else(|| Error::Protocol("missing created event".into()))?;
        let mut merged = serde_json::to_value(event)?;
        merge_json(&mut merged, created);
        Ok(serde_json::from_value(merged)?)
    }

    pub async fn update_event(&self, account_id: &str, id: &str, patch: Value) -> Result<(), Error> {
        let mut update = BTreeMap::new();
        update.insert(id.to_string(), patch);
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "CalendarEvent/set".into(),
                    json!({ "accountId": account_id, "update": update }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        if let Some(err) = result.get("notUpdated").and_then(|v| v.get(id)) {
            return Err(Error::Protocol(format!("event not updated: {err}")));
        }
        Ok(())
    }

    pub async fn destroy_event(&self, account_id: &str, id: &str) -> Result<(), Error> {
        let resp = self
            .call(
                self.using(),
                vec![MethodCall(
                    "CalendarEvent/set".into(),
                    json!({ "accountId": account_id, "destroy": [id] }),
                    "c0".into(),
                )],
            )
            .await?;
        let result = resp.result_for("c0")?;
        if let Some(err) = result.get("notDestroyed").and_then(|v| v.get(id)) {
            return Err(Error::Protocol(format!("event not destroyed: {err}")));
        }
        Ok(())
    }
}

fn merge_json(base: &mut Value, patch: &Value) {
    if let (Some(base_obj), Some(patch_obj)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
}
