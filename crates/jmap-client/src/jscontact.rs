//! A pragmatic subset of JSContact (RFC 9553) plus the JMAP Contacts
//! extension layered on top of it, scoped to what the calendar UI needs:
//! a display name and birth anniversaries. Unknown/extension properties
//! are preserved via `extra` so round-tripping never silently drops data.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jscalendar::Id;

fn card_type() -> String {
    "Card".to_string()
}
fn version_1() -> String {
    "1.0".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NameComponent {
    #[serde(rename = "@type", default = "name_component_type")]
    pub type_: String,
    pub kind: String,
    pub value: String,
}

fn name_component_type() -> String {
    "NameComponent".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NameProperty {
    #[serde(rename = "@type", default = "name_type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<NameComponent>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn name_type() -> String {
    "Name".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmailAddress {
    #[serde(rename = "@type", default = "email_type")]
    pub type_: String,
    pub address: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn email_type() -> String {
    "EmailAddress".to_string()
}

/// RFC 9553 §2.4.1: either a full timestamp or a (possibly year-less,
/// for privacy) partial calendar date.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "@type")]
pub enum AnniversaryDate {
    Timestamp {
        utc: String,
    },
    PartialDate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        month: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        day: Option<u32>,
    },
}

impl AnniversaryDate {
    /// The (year, month, day) this anniversary falls on, year being `None`
    /// when the card owner chose not to disclose it.
    pub fn month_day_year(&self) -> Option<(u32, u32, Option<i32>)> {
        match self {
            AnniversaryDate::Timestamp { utc } => {
                let date = utc.get(0..10)?;
                let y: i32 = date.get(0..4)?.parse().ok()?;
                let m: u32 = date.get(5..7)?.parse().ok()?;
                let d: u32 = date.get(8..10)?.parse().ok()?;
                Some((m, d, Some(y)))
            }
            AnniversaryDate::PartialDate { year, month, day } => Some(((*month)?, (*day)?, *year)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anniversary {
    #[serde(rename = "@type", default = "anniversary_type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub date: AnniversaryDate,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn anniversary_type() -> String {
    "Anniversary".to_string()
}

/// A JSContact `Card`, as returned by JMAP `ContactCard/get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    #[serde(rename = "@type", default = "card_type")]
    pub type_: String,
    #[serde(default = "version_1")]
    pub version: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub uid: Id,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<NameProperty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emails: Option<BTreeMap<Id, EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anniversaries: Option<BTreeMap<Id, Anniversary>>,

    /// JMAP Contacts: which address book(s) this card belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "addressBookIds")]
    pub address_book_ids: Option<BTreeMap<Id, bool>>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Card {
    pub fn new(uid: impl Into<Id>) -> Self {
        Self {
            type_: card_type(),
            version: version_1(),
            id: None,
            uid: uid.into(),
            kind: None,
            name: None,
            emails: None,
            anniversaries: None,
            address_book_ids: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn display_name(&self) -> String {
        self.name
            .as_ref()
            .and_then(|n| n.full.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.uid.clone())
    }

    pub fn primary_email(&self) -> Option<&str> {
        self.emails
            .as_ref()?
            .values()
            .next()
            .map(|e| e.address.as_str())
    }
}

/// An `AddressBook`, the contacts analogue of `Calendar`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressBook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub name: String,
    #[serde(default, rename = "sortOrder")]
    pub sort_order: u32,
    #[serde(default = "default_true", rename = "isSubscribed")]
    pub is_subscribed: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn default_true() -> bool {
    true
}

/// A contact's birthday, resolved to a concrete display name and a
/// month/day (with an optional year for computing an age).
#[derive(Debug, Clone)]
pub struct Birthday {
    pub uid: Id,
    pub name: String,
    pub month: u32,
    pub day: u32,
    pub birth_year: Option<i32>,
}

impl Birthday {
    fn occurrence_in_year(&self, year: i32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(year, self.month, self.day)
    }

    pub fn turns_years_old(&self, occurrence_year: i32) -> Option<i32> {
        self.birth_year.map(|by| occurrence_year - by)
    }
}

fn birthdays_from_card(card: &Card) -> Vec<Birthday> {
    let Some(anniversaries) = &card.anniversaries else {
        return Vec::new();
    };
    anniversaries
        .values()
        .filter(|a| a.kind.as_deref() == Some("birth"))
        .filter_map(|a| {
            let (month, day, year) = a.date.month_day_year()?;
            Some(Birthday {
                uid: card.uid.clone(),
                name: card.display_name(),
                month,
                day,
                birth_year: year,
            })
        })
        .collect()
}

/// A birthday occurrence landing on a specific date, ready for display.
pub struct BirthdayOccurrence {
    pub date: NaiveDate,
    pub name: String,
    pub uid: Id,
    pub turns: Option<i32>,
}

/// Expand every card's birthday anniversaries into concrete occurrences
/// intersecting `[range_start, range_end)`, recurring annually.
pub fn expand_birthdays(
    cards: &[Card],
    range_start: NaiveDate,
    range_end: NaiveDate,
) -> Vec<BirthdayOccurrence> {
    let mut out = Vec::new();
    for card in cards {
        for bday in birthdays_from_card(card) {
            for year in (range_start.year() - 1)..=(range_end.year() + 1) {
                if let Some(date) = bday.occurrence_in_year(year) {
                    if date >= range_start && date < range_end {
                        out.push(BirthdayOccurrence {
                            date,
                            name: bday.name.clone(),
                            uid: bday.uid.clone(),
                            turns: bday.turns_years_old(year),
                        });
                    }
                }
            }
        }
    }
    out.sort_by_key(|o| o.date);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_with_birthday(date: AnniversaryDate) -> Card {
        let mut anniversaries = BTreeMap::new();
        anniversaries.insert(
            "a1".to_string(),
            Anniversary {
                type_: "Anniversary".to_string(),
                kind: Some("birth".to_string()),
                date,
                extra: BTreeMap::new(),
            },
        );
        Card {
            type_: card_type(),
            version: version_1(),
            id: Some("c1".to_string()),
            uid: "c1-uid".to_string(),
            kind: None,
            name: Some(NameProperty {
                type_: name_type(),
                full: Some("Ada Lovelace".to_string()),
                components: vec![],
                extra: BTreeMap::new(),
            }),
            emails: None,
            anniversaries: Some(anniversaries),
            address_book_ids: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn expands_partial_date_without_year() {
        let card = card_with_birthday(AnniversaryDate::PartialDate {
            year: None,
            month: Some(12),
            day: Some(10),
        });
        let occs = expand_birthdays(
            &[card],
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        );
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].date, NaiveDate::from_ymd_opt(2026, 12, 10).unwrap());
        assert_eq!(occs[0].name, "Ada Lovelace");
        assert_eq!(occs[0].turns, None);
    }

    #[test]
    fn expands_full_timestamp_with_age() {
        let card = card_with_birthday(AnniversaryDate::Timestamp {
            utc: "1815-12-10T00:00:00Z".to_string(),
        });
        let occs = expand_birthdays(
            &[card],
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        );
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].turns, Some(211));
    }

    #[test]
    fn ignores_non_birth_anniversaries() {
        let mut card = card_with_birthday(AnniversaryDate::PartialDate {
            year: None,
            month: Some(6),
            day: Some(1),
        });
        card.anniversaries
            .as_mut()
            .unwrap()
            .get_mut("a1")
            .unwrap()
            .kind = Some("wedding".to_string());
        let occs = expand_birthdays(
            &[card],
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        );
        assert!(occs.is_empty());
    }

    #[test]
    fn range_spanning_year_boundary_still_matches() {
        let card = card_with_birthday(AnniversaryDate::PartialDate {
            year: None,
            month: Some(1),
            day: Some(2),
        });
        let occs = expand_birthdays(
            &[card],
            NaiveDate::from_ymd_opt(2026, 12, 28).unwrap(),
            NaiveDate::from_ymd_opt(2027, 1, 4).unwrap(),
        );
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].date, NaiveDate::from_ymd_opt(2027, 1, 2).unwrap());
    }
}
