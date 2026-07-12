//! JSCalendar data model (RFC 8984) and the JMAP Calendars extensions
//! (RFC 9670) layered on top of it.
//!
//! Unknown/extension properties are preserved via `extra` so that round
//! tripping through this client never silently drops server data.

use std::collections::BTreeMap;

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Id = String;
/// A JSCalendar "String[Boolean]" set, e.g. keywords, categories.
pub type BoolSet = BTreeMap<String, bool>;
/// A JSCalendar patch object (RFC 8620 section 1.6.3): JSON-pointer-ish
/// dotted keys mapped to replacement values, or `null` to remove.
pub type PatchObject = BTreeMap<String, Value>;

/// JSCalendar `LocalDateTime`: a date-time with no attached time zone,
/// formatted as `YYYY-MM-DDTHH:MM:SS`. Interpretation depends on the
/// object's `timeZone` property.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalDateTime(pub String);

impl LocalDateTime {
    pub fn from_naive(dt: NaiveDateTime) -> Self {
        Self(dt.format("%Y-%m-%dT%H:%M:%S").to_string())
    }

    pub fn to_naive(&self) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(&self.0, "%Y-%m-%dT%H:%M:%S").ok()
    }

    pub fn date(&self) -> Option<NaiveDate> {
        self.to_naive().map(|d| d.date())
    }
}

fn default_true() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecurrenceRule {
    #[serde(rename = "@type", default = "rr_type")]
    pub type_: String,
    pub frequency: Frequency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<LocalDateTime>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "byDay")]
    pub by_day: Vec<NDay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "byMonthDay")]
    pub by_month_day: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "byMonth")]
    pub by_month: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "bySetPosition")]
    pub by_set_position: Vec<i32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn rr_type() -> String {
    "RecurrenceRule".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    #[default]
    Daily,
    Weekly,
    Monthly,
    Yearly,
    Hourly,
    Minutely,
    Secondly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NDay {
    #[serde(rename = "@type", default = "nday_type")]
    pub type_: String,
    pub day: Weekday,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "nthOfPeriod")]
    pub nth_of_period: Option<i32>,
}

fn nday_type() -> String {
    "NDay".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Location {
    #[serde(rename = "@type", default = "location_type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "relativeTo")]
    pub relative_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "timeZone")]
    pub time_zone: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn location_type() -> String {
    "Location".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VirtualLocation {
    #[serde(rename = "@type", default = "vlocation_type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub uri: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn vlocation_type() -> String {
    "VirtualLocation".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Link {
    #[serde(rename = "@type", default = "link_type")]
    pub type_: String,
    pub href: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn link_type() -> String {
    "Link".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Participant {
    #[serde(rename = "@type", default = "participant_type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "participationStatus")]
    pub participation_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn participant_type() -> String {
    "Participant".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "@type")]
pub enum Trigger {
    #[serde(rename = "OffsetTrigger")]
    Offset {
        offset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relative_to: Option<String>,
    },
    #[serde(rename = "AbsoluteTrigger")]
    Absolute { when: LocalDateTime },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    #[serde(rename = "@type", default = "alert_type")]
    pub type_: String,
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn alert_type() -> String {
    "Alert".to_string()
}

/// A JSCalendar `Event` object as used over JMAP (RFC 9670 §4): the base
/// RFC 8984 event properties plus the JMAP CalendarEvent extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    #[serde(rename = "@type", default = "event_type")]
    pub type_: String,

    /// Server-assigned JMAP id. Absent when creating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,

    pub uid: Id,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "is_false", rename = "showWithoutTime")]
    pub show_without_time: bool,

    pub start: LocalDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "timeZone")]
    pub time_zone: Option<String>,
    #[serde(default = "default_duration", skip_serializing_if = "is_zero_duration")]
    pub duration: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "freeBusyStatus")]
    pub free_busy_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<BTreeMap<Id, Location>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "virtualLocations")]
    pub virtual_locations: Option<BTreeMap<Id, VirtualLocation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<BTreeMap<Id, Link>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<BoolSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<BoolSet>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participants: Option<BTreeMap<Id, Participant>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "useDefaultAlerts")]
    pub use_default_alerts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alerts: Option<BTreeMap<Id, Alert>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "recurrenceRules")]
    pub recurrence_rules: Option<Vec<RecurrenceRule>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "recurrenceOverrides")]
    pub recurrence_overrides: Option<BTreeMap<LocalDateTime, PatchObject>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "recurrenceId")]
    pub recurrence_id: Option<LocalDateTime>,

    /// JMAP Calendars: which calendar(s) this event belongs to (usually one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "calendarIds")]
    pub calendar_ids: Option<BTreeMap<Id, bool>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "isDraft")]
    pub is_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "isOrigin")]
    pub is_origin: Option<bool>,
    /// Server-computed UTC instant of `start`, informational/read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "utcStart")]
    pub utc_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "utcEnd")]
    pub utc_end: Option<String>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn event_type() -> String {
    "Event".to_string()
}

fn default_duration() -> String {
    "PT0S".to_string()
}

fn is_zero_duration(d: &str) -> bool {
    d == "PT0S"
}

impl CalendarEvent {
    pub fn new(uid: impl Into<Id>, start: LocalDateTime) -> Self {
        Self {
            type_: event_type(),
            id: None,
            uid: uid.into(),
            created: None,
            updated: None,
            sequence: None,
            title: None,
            description: None,
            show_without_time: false,
            start,
            time_zone: None,
            duration: default_duration(),
            status: None,
            free_busy_status: None,
            privacy: None,
            color: None,
            priority: None,
            locations: None,
            virtual_locations: None,
            links: None,
            keywords: None,
            categories: None,
            participants: None,
            use_default_alerts: None,
            alerts: None,
            recurrence_rules: None,
            recurrence_overrides: None,
            recurrence_id: None,
            calendar_ids: None,
            is_draft: None,
            is_origin: None,
            utc_start: None,
            utc_end: None,
            extra: BTreeMap::new(),
        }
    }
}

/// Rights a principal has on a `Calendar` (RFC 9670 §2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalendarRights {
    #[serde(default, rename = "mayReadFreeBusy")]
    pub may_read_free_busy: bool,
    #[serde(default, rename = "mayReadItems")]
    pub may_read_items: bool,
    #[serde(default, rename = "mayAddItems")]
    pub may_add_items: bool,
    #[serde(default, rename = "mayUpdatePrivate")]
    pub may_update_private: bool,
    #[serde(default, rename = "mayUpdateOwn")]
    pub may_update_own: bool,
    #[serde(default, rename = "mayUpdateAll")]
    pub may_update_all: bool,
    #[serde(default, rename = "mayRemoveOwn")]
    pub may_remove_own: bool,
    #[serde(default, rename = "mayRemoveAll")]
    pub may_remove_all: bool,
    #[serde(default, rename = "mayAdmin")]
    pub may_admin: bool,
    #[serde(default, rename = "mayDelete")]
    pub may_delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calendar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, rename = "sortOrder")]
    pub sort_order: u32,
    #[serde(default = "default_true", rename = "isSubscribed")]
    pub is_subscribed: bool,
    #[serde(default = "default_true", rename = "isVisible")]
    pub is_visible: bool,
    #[serde(default, rename = "myRights")]
    pub my_rights: Option<CalendarRights>,
    /// Sharing grants: JMAP account id of the other user → the rights
    /// granted to them (RFC 9670 §2, `Calendar/set` `shareWith`). Only
    /// meaningful (and only settable) by the calendar's owner.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "shareWith")]
    pub share_with: Option<BTreeMap<Id, CalendarRights>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Calendar {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            description: None,
            color: None,
            sort_order: 0,
            is_subscribed: true,
            is_visible: true,
            my_rights: None,
            share_with: None,
            extra: BTreeMap::new(),
        }
    }
}

impl CalendarRights {
    /// Read-only access: see events/free-busy, nothing else.
    pub fn view_only() -> Self {
        Self {
            may_read_free_busy: true,
            may_read_items: true,
            ..Default::default()
        }
    }

    /// Read/write access to the calendar's content — not the calendar's own
    /// admin/sharing/deletion rights, which this simple two-tier model
    /// never grants.
    pub fn can_edit() -> Self {
        Self {
            may_read_free_busy: true,
            may_read_items: true,
            may_add_items: true,
            may_update_private: true,
            may_update_own: true,
            may_update_all: true,
            may_remove_own: true,
            may_remove_all: true,
            ..Default::default()
        }
    }
}
