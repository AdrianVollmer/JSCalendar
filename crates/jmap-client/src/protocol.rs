//! JMAP Core protocol plumbing (RFC 8620): session resource, the
//! request/response envelope, and method-call/back-reference helpers.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY_CORE: &str = "urn:ietf:params:jmap:core";
pub const CAPABILITY_CALENDARS: &str = "urn:ietf:params:jmap:calendars";
pub const CAPABILITY_CONTACTS: &str = "urn:ietf:params:jmap:contacts";

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub capabilities: Value,
    pub accounts: HashMap<String, Account>,
    #[serde(rename = "primaryAccounts")]
    pub primary_accounts: HashMap<String, String>,
    pub username: String,
    #[serde(rename = "apiUrl")]
    pub api_url: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: String,
    #[serde(rename = "uploadUrl")]
    pub upload_url: String,
    #[serde(rename = "eventSourceUrl", default)]
    pub event_source_url: Option<String>,
    pub state: String,
}

impl Session {
    /// The account id to use for calendar operations: the primary account
    /// for the calendars capability if advertised, else the first account
    /// that supports it, else the first account at all.
    pub fn calendars_account_id(&self) -> Option<&str> {
        self.account_id_for(CAPABILITY_CALENDARS)
    }

    /// The account id to use for contacts operations, or `None` if the
    /// server doesn't advertise JMAP Contacts support at all.
    pub fn contacts_account_id(&self) -> Option<&str> {
        if let Some(id) = self.primary_accounts.get(CAPABILITY_CONTACTS) {
            return Some(id.as_str());
        }
        self.accounts
            .iter()
            .find(|(_, a)| a.account_capabilities.contains_key(CAPABILITY_CONTACTS))
            .map(|(id, _)| id.as_str())
    }

    fn account_id_for(&self, capability: &str) -> Option<&str> {
        if let Some(id) = self.primary_accounts.get(capability) {
            return Some(id.as_str());
        }
        self.accounts
            .iter()
            .find(|(_, a)| a.account_capabilities.contains_key(capability))
            .map(|(id, _)| id.as_str())
            .or_else(|| self.accounts.keys().next().map(|s| s.as_str()))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub name: String,
    #[serde(rename = "isPersonal", default)]
    pub is_personal: bool,
    #[serde(rename = "isReadOnly", default)]
    pub is_read_only: bool,
    #[serde(rename = "accountCapabilities", default)]
    pub account_capabilities: HashMap<String, Value>,
}

/// A single entry in `methodCalls`/`methodResponses`: `[name, arguments, id]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodCall(pub String, pub Value, pub String);

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub using: Vec<String>,
    #[serde(rename = "methodCalls")]
    pub method_calls: Vec<MethodCall>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    #[serde(rename = "methodResponses")]
    pub method_responses: Vec<MethodCall>,
    #[serde(rename = "sessionState", default)]
    pub session_state: Option<String>,
}

impl Response {
    /// Find the first response with the given call id, erroring out if the
    /// server returned a JMAP `error` method response for it.
    pub fn result_for(&self, call_id: &str) -> Result<&Value, crate::Error> {
        let mc = self
            .method_responses
            .iter()
            .find(|mc| mc.2 == call_id)
            .ok_or_else(|| crate::Error::Protocol(format!("no response for call {call_id}")))?;
        if mc.0 == "error" {
            return Err(crate::Error::Protocol(format!(
                "method error for {call_id}: {}",
                mc.1
            )));
        }
        Ok(&mc.1)
    }
}

/// A JMAP result reference (RFC 8620 §3.7), used to chain e.g.
/// `CalendarEvent/query` -> `CalendarEvent/get` in one round trip.
pub fn result_reference(call_id: &str, path: &str) -> Value {
    serde_json::json!({
        "resultOf": call_id,
        "name": "CalendarEvent/get",
        "path": path,
    })
}
