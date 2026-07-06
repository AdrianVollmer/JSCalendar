//! A deliberately minimal, in-memory JMAP server good enough to drive
//! JSCalendar end-to-end without a real account: Core session discovery,
//! `Calendar`/`CalendarEvent` (JMAP for Calendars), and `AddressBook`/
//! `ContactCard` (JMAP Contacts). Credentials are never checked — any
//! username/password/token is accepted — since this exists purely for
//! local testing and demos.
//!
//! Deliberately implemented against raw JSON rather than the `jmap-client`
//! types: it exists to exercise the real wire protocol as an independent
//! black box, the same way a real server would.

use std::sync::Mutex;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

struct Db {
    calendars: Vec<Value>,
    events: Vec<Value>,
    address_books: Vec<Value>,
    cards: Vec<Value>,
    next_id: u64,
}

type AppState = std::sync::Arc<Mutex<Db>>;

fn iso(dt: chrono::DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn seed() -> Db {
    let now = Utc::now();
    let today_9am = now.date_naive().and_hms_opt(9, 0, 0).unwrap().and_utc();
    let day = |n: i64| today_9am + Duration::days(n);

    let calendars = vec![
        json!({ "id": "cal1", "name": "Personal", "color": "#6366f1", "sortOrder": 0, "isSubscribed": true, "isVisible": true }),
        json!({ "id": "cal2", "name": "Work", "color": "#0ea5e9", "sortOrder": 1, "isSubscribed": true, "isVisible": true }),
    ];

    let events = vec![
        json!({
            "@type": "Event", "id": "evt1", "uid": "evt1-uid",
            "title": "Team standup", "description": "Daily sync",
            "start": iso(day(0)), "timeZone": "Europe/Berlin", "duration": "PT15M",
            "calendarIds": { "cal1": true },
            "recurrenceRules": [{ "@type": "RecurrenceRule", "frequency": "daily", "interval": 1 }],
        }),
        json!({
            "@type": "Event", "id": "evt2", "uid": "evt2-uid",
            "title": "Design review",
            "start": iso(day(1) + Duration::hours(5)), "timeZone": "Europe/Berlin", "duration": "PT1H",
            "calendarIds": { "cal2": true },
            "locations": { "loc1": { "@type": "Location", "name": "Room 4B" } },
        }),
        json!({
            "@type": "Event", "id": "evt3", "uid": "evt3-uid",
            "title": "Company offsite",
            "start": iso(day(3).date_naive().and_hms_opt(0,0,0).unwrap().and_utc()),
            "showWithoutTime": true, "duration": "P2D",
            "calendarIds": { "cal2": true },
        }),
        json!({
            "@type": "Event", "id": "evt4", "uid": "evt4-uid",
            "title": "1:1 with manager",
            "start": iso(day(2) + Duration::hours(6)), "timeZone": "Europe/Berlin", "duration": "PT30M",
            "calendarIds": { "cal2": true },
            "recurrenceRules": [{ "@type": "RecurrenceRule", "frequency": "weekly", "interval": 1 }],
        }),
    ];

    let address_books =
        vec![json!({ "id": "ab1", "name": "Contacts", "sortOrder": 0, "isSubscribed": true })];

    let this_year = now
        .date_naive()
        .format("%Y")
        .to_string()
        .parse::<i32>()
        .unwrap_or(2026);
    let cards = vec![
        json!({
            "@type": "Card", "version": "1.0", "id": "card1", "uid": "card1-uid",
            "name": { "@type": "Name", "full": "Ada Lovelace" },
            "emails": { "e1": { "@type": "EmailAddress", "address": "ada@example.com" } },
            "anniversaries": { "a1": { "@type": "Anniversary", "kind": "birth",
                "date": { "@type": "Timestamp", "utc": format!("{}-{}T00:00:00Z", this_year - 41, now.format("%m-%d")) } } },
            "addressBookIds": { "ab1": true },
        }),
        json!({
            "@type": "Card", "version": "1.0", "id": "card2", "uid": "card2-uid",
            "name": { "@type": "Name", "full": "Grace Hopper" },
            "emails": { "e1": { "@type": "EmailAddress", "address": "grace@example.com" } },
            "anniversaries": { "a1": { "@type": "Anniversary", "kind": "birth",
                "date": { "@type": "PartialDate", "month": (day(9).date_naive().month()), "day": (day(9).date_naive().day()) } } },
            "addressBookIds": { "ab1": true },
        }),
        json!({
            "@type": "Card", "version": "1.0", "id": "card3", "uid": "card3-uid",
            "name": { "@type": "Name", "full": "Alan Turing" },
            "addressBookIds": { "ab1": true },
        }),
    ];

    Db {
        calendars,
        events,
        address_books,
        cards,
        next_id: 100,
    }
}

use chrono::Datelike;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mock_jmap_server=info".into()),
        )
        .init();

    let state: AppState = std::sync::Arc::new(Mutex::new(seed()));

    let app = Router::new()
        .route("/.well-known/jmap", get(session))
        .route("/api", post(api))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9090);
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string());
    let listener = tokio::net::TcpListener::bind((bind_addr.as_str(), port))
        .await
        .expect("failed to bind listener");
    tracing::info!("mock JMAP server on http://{bind_addr}:{port} — any credentials are accepted");
    axum::serve(listener, app).await.expect("server error");
}

async fn session(headers: HeaderMap) -> Json<Value> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:9090");
    let base = format!("http://{host}");
    Json(json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {},
            "urn:ietf:params:jmap:calendars": {},
            "urn:ietf:params:jmap:contacts": {},
        },
        "accounts": {
            "acc1": {
                "name": "demo@example.com",
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {
                    "urn:ietf:params:jmap:calendars": {},
                    "urn:ietf:params:jmap:contacts": {},
                },
            },
        },
        "primaryAccounts": {
            "urn:ietf:params:jmap:calendars": "acc1",
            "urn:ietf:params:jmap:contacts": "acc1",
        },
        "username": "demo@example.com",
        "apiUrl": format!("{base}/api"),
        "downloadUrl": format!("{base}/download/{{blobId}}"),
        "uploadUrl": format!("{base}/upload"),
        "state": "s1",
    }))
}

#[derive(Deserialize)]
struct JmapRequest {
    #[serde(rename = "methodCalls")]
    method_calls: Vec<(String, Value, String)>,
}

async fn api(State(state): State<AppState>, Json(req): Json<JmapRequest>) -> Response {
    let mut db = state.lock().unwrap();
    let mut responses: Vec<(String, Value, String)> = Vec::new();

    for (name, mut args, id) in req.method_calls {
        resolve_result_refs(&mut args, &responses);
        let result = dispatch(&mut db, &name, &args);
        responses.push((result.0, result.1, id));
    }

    let body = json!({ "methodResponses": responses, "sessionState": "s1" });
    (StatusCode::OK, Json(body)).into_response()
}

/// Resolves JMAP "result reference" arguments (RFC 8620 §3.7): any key
/// starting with `#` is replaced by the named path (only single-segment
/// paths like `/ids` are needed here) plucked from an earlier response in
/// the same request.
fn resolve_result_refs(args: &mut Value, prior: &[(String, Value, String)]) {
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    let ref_keys: Vec<String> = obj.keys().filter(|k| k.starts_with('#')).cloned().collect();
    for key in ref_keys {
        if let Some(reference) = obj.get(&key).cloned() {
            let result_of = reference.get("resultOf").and_then(|v| v.as_str());
            let path = reference.get("path").and_then(|v| v.as_str());
            if let (Some(result_of), Some(path)) = (result_of, path) {
                if let Some((_, result, _)) = prior.iter().find(|(_, _, cid)| cid == result_of) {
                    let field = path.trim_start_matches('/');
                    if let Some(val) = result.get(field) {
                        let real_key = key.trim_start_matches('#').to_string();
                        obj.insert(real_key, val.clone());
                    }
                }
            }
        }
        obj.remove(&key);
    }
}

fn dispatch(db: &mut Db, name: &str, args: &Value) -> (String, Value) {
    let account_id = args
        .get("accountId")
        .and_then(|v| v.as_str())
        .unwrap_or("acc1")
        .to_string();
    match name {
        "Calendar/get" => (name.into(), get_response(&account_id, &db.calendars)),
        "Calendar/set" => (
            name.into(),
            set_response(&account_id, &mut db.calendars, &mut db.next_id, args, "cal"),
        ),
        "CalendarEvent/get" => (
            name.into(),
            get_by_ids_response(&account_id, &db.events, args),
        ),
        "CalendarEvent/set" => (
            name.into(),
            set_response(&account_id, &mut db.events, &mut db.next_id, args, "evt"),
        ),
        "CalendarEvent/query" => (
            name.into(),
            query_events_response(&account_id, &db.events, args),
        ),
        "AddressBook/get" => (name.into(), get_response(&account_id, &db.address_books)),
        "ContactCard/get" => {
            tracing::info!(
                "ContactCard/get requested (properties: {:?})",
                args.get("properties")
            );
            (
                name.into(),
                get_by_ids_response(&account_id, &db.cards, args),
            )
        }
        other => (
            "error".into(),
            json!({ "type": "unknownMethod", "description": format!("mock server does not implement {other}") }),
        ),
    }
}

fn get_response(account_id: &str, list: &[Value]) -> Value {
    json!({ "accountId": account_id, "state": "s1", "list": list, "notFound": [] })
}

fn get_by_ids_response(account_id: &str, list: &[Value], args: &Value) -> Value {
    let ids = args.get("ids").and_then(|v| v.as_array());
    let filtered: Vec<Value> = match ids {
        None => list.to_vec(),
        Some(ids) => {
            let wanted: Vec<&str> = ids.iter().filter_map(|v| v.as_str()).collect();
            list.iter()
                .filter(|item| {
                    item.get("id")
                        .and_then(|v| v.as_str())
                        .map(|id| wanted.contains(&id))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        }
    };
    json!({ "accountId": account_id, "state": "s1", "list": filtered, "notFound": [] })
}

fn set_response(
    account_id: &str,
    list: &mut Vec<Value>,
    next_id: &mut u64,
    args: &Value,
    id_prefix: &str,
) -> Value {
    let mut created = serde_json::Map::new();
    let mut updated = serde_json::Map::new();
    let mut destroyed: Vec<Value> = Vec::new();

    if let Some(create) = args.get("create").and_then(|v| v.as_object()) {
        for (client_key, value) in create {
            let mut obj = value.clone();
            let new_id = format!("{id_prefix}{next_id}");
            *next_id += 1;
            if let Some(map) = obj.as_object_mut() {
                map.insert("id".to_string(), json!(new_id));
            }
            created.insert(client_key.clone(), json!({ "id": new_id }));
            list.push(obj);
        }
    }
    if let Some(update) = args.get("update").and_then(|v| v.as_object()) {
        for (item_id, patch) in update {
            if let Some(existing) = list
                .iter_mut()
                .find(|item| item.get("id").and_then(|v| v.as_str()) == Some(item_id.as_str()))
            {
                if let (Some(existing_map), Some(patch_map)) =
                    (existing.as_object_mut(), patch.as_object())
                {
                    for (k, v) in patch_map {
                        existing_map.insert(k.clone(), v.clone());
                    }
                }
            }
            updated.insert(item_id.clone(), Value::Null);
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(|v| v.as_array()) {
        let ids: Vec<&str> = destroy.iter().filter_map(|v| v.as_str()).collect();
        list.retain(|item| {
            item.get("id")
                .and_then(|v| v.as_str())
                .map(|id| !ids.contains(&id))
                .unwrap_or(true)
        });
        destroyed = destroy.clone();
    }

    json!({
        "accountId": account_id,
        "oldState": "s1",
        "newState": "s2",
        "created": created,
        "updated": updated,
        "destroyed": destroyed,
    })
}

/// Matches the JMAP Calendars `CalendarEvent/query` filter semantics
/// closely enough for demo purposes: `after`/`before` bound the event's
/// own `start`/`duration`, and recurring events (which may recur into the
/// range regardless of their own `start`) always match.
fn query_events_response(account_id: &str, events: &[Value], args: &Value) -> Value {
    let filter = args.get("filter");
    let after = filter.and_then(|f| f.get("after")).and_then(|v| v.as_str());
    let before = filter
        .and_then(|f| f.get("before"))
        .and_then(|v| v.as_str());
    let in_calendars: Option<Vec<&str>> = filter
        .and_then(|f| f.get("inCalendars"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect());

    let matches = |ev: &Value| -> bool {
        if let Some(cals) = &in_calendars {
            let event_cals: Vec<&str> = ev
                .get("calendarIds")
                .and_then(|v| v.as_object())
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            if !event_cals.iter().any(|c| cals.contains(c)) {
                return false;
            }
        }
        if ev.get("recurrenceRules").is_some() {
            return true;
        }
        let Some(start) = ev.get("start").and_then(|v| v.as_str()) else {
            return false;
        };
        let matches_before = before
            .map(|b| start < b.trim_end_matches('Z'))
            .unwrap_or(true);
        let matches_after = after
            .map(|a| start >= &a[..a.len().min(19)])
            .unwrap_or(true);
        matches_before && matches_after
    };

    let ids: Vec<Value> = events
        .iter()
        .filter(|e| matches(e))
        .filter_map(|e| e.get("id").cloned())
        .collect();
    let total = ids.len();
    json!({
        "accountId": account_id,
        "queryState": "q1",
        "canCalculateChanges": false,
        "position": 0,
        "ids": ids,
        "total": total,
    })
}
