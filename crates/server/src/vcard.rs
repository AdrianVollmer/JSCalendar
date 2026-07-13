//! Minimal vCard (RFC 6350, and the common RFC 2426 3.0 shapes exported by
//! most address books) parser for bulk contact import.
//!
//! Line folding and the `NAME;PARAM=value:VALUE` content-line grammar are
//! shared with iCalendar (both are in the same content-line-family of
//! RFCs), so this reuses `crate::ics`'s folding/line-splitting helpers
//! rather than re-implementing them.
//!
//! Deliberately narrow, matching what a single contact-create already
//! supports: a name (`FN`, falling back to `N`), one email address
//! (preferring one marked `TYPE=pref`), and a birthday (`BDAY`, with or
//! without a year). Everything else in a vCard (phone numbers, addresses,
//! photos, notes, ...) is ignored.

use chrono::NaiveDate;
use jmap_client::jscontact::Card;

use crate::ics::{escape_text, fold_line, parse_line, unescape_text, unfold, Params};

pub struct ParsedContact {
    pub name: String,
    pub email: Option<String>,
    /// `true` when the source had a real year, `false` for a
    /// year-omitted birthday (vCard 4's `--MMDD` form) — mirrors the
    /// "hide birth year" checkbox on the regular contact form.
    pub birthday: Option<(NaiveDate, bool)>,
}

pub fn parse_vcards(text: &str) -> Vec<ParsedContact> {
    let mut out = Vec::new();
    let mut current: Option<RawContact> = None;

    for line in unfold(text) {
        let Some((name, params, value)) = parse_line(&line) else {
            continue;
        };
        match name.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VCARD") => current = Some(RawContact::default()),
            "END" if value.eq_ignore_ascii_case("VCARD") => {
                if let Some(raw) = current.take() {
                    if let Some(contact) = raw.into_contact() {
                        out.push(contact);
                    }
                }
            }
            _ => {
                if let Some(raw) = current.as_mut() {
                    raw.set(&name, &params, &value);
                }
            }
        }
    }
    out
}

/// Serializes a single contact card to a standalone vCard — the write-side
/// counterpart of `parse_vcards`, just as narrow: display name, every
/// email address, and a birth anniversary if there is one. Everything
/// else JSContact can carry (phone numbers, addresses, photos, notes, …)
/// isn't modeled by this app in the first place, so there's nothing to
/// export for it.
pub fn to_vcard(card: &Card) -> String {
    let mut lines = vec!["BEGIN:VCARD".to_string(), "VERSION:3.0".to_string()];
    let name = card
        .name
        .as_ref()
        .and_then(|n| n.full.as_deref())
        .unwrap_or_default();
    lines.push(format!("FN:{}", escape_text(name)));
    lines.push(format!("N:{};;;;", escape_text(name)));
    if let Some(emails) = &card.emails {
        for email in emails.values() {
            lines.push(format!("EMAIL:{}", escape_text(&email.address)));
        }
    }
    if let Some(birth) = card
        .anniversaries
        .as_ref()
        .and_then(|m| m.values().find(|a| a.kind.as_deref() == Some("birth")))
    {
        if let Some((month, day, year)) = birth.date.month_day_year() {
            let value = match year {
                Some(y) => format!("{y:04}-{month:02}-{day:02}"),
                None => format!("--{month:02}{day:02}"),
            };
            lines.push(format!("BDAY:{value}"));
        }
    }
    lines.push(format!("UID:{}", escape_text(&card.uid)));
    lines.push("END:VCARD".to_string());
    lines.iter().map(|l| fold_line(l)).collect()
}

#[derive(Default)]
struct RawContact {
    fn_: Option<String>,
    n: Option<String>,
    email: Option<String>,
    email_is_preferred: bool,
    bday: Option<(String, Params)>,
}

impl RawContact {
    fn set(&mut self, name: &str, params: &[(String, String)], value: &str) {
        match name {
            "FN" => self.fn_ = Some(unescape_text(value)),
            "N" => self.n = Some(unescape_text(value)),
            "EMAIL" => {
                let is_preferred = params
                    .iter()
                    .any(|(k, v)| (k == "TYPE" && v.eq_ignore_ascii_case("pref")) || k == "PREF");
                if is_preferred || self.email.is_none() {
                    self.email = Some(value.trim().to_string());
                    self.email_is_preferred = is_preferred;
                }
            }
            "BDAY" => self.bday = Some((value.to_string(), params.to_vec())),
            _ => {}
        }
    }

    fn into_contact(self) -> Option<ParsedContact> {
        let name = self
            .fn_
            .or_else(|| self.n.as_deref().map(name_from_n))
            .unwrap_or_default();
        let name = name.trim().to_string();
        if name.is_empty() {
            return None;
        }
        let birthday = self
            .bday
            .and_then(|(raw, params)| parse_bday(&raw, &params));
        Some(ParsedContact {
            name,
            email: self.email,
            birthday,
        })
    }
}

/// Builds a display name from an `N` value (`Family;Given;Additional;
/// Prefix;Suffix`) when there's no `FN`, e.g. `"Lovelace;Ada;;;"` ->
/// `"Ada Lovelace"`.
fn name_from_n(n: &str) -> String {
    let parts: Vec<&str> = n.split(';').collect();
    let family = parts.first().copied().unwrap_or("").trim();
    let given = parts.get(1).copied().unwrap_or("").trim();
    match (given.is_empty(), family.is_empty()) {
        (false, false) => format!("{given} {family}"),
        (false, true) => given.to_string(),
        (true, false) => family.to_string(),
        (true, true) => String::new(),
    }
}

fn parse_bday(raw: &str, params: &[(String, String)]) -> Option<(NaiveDate, bool)> {
    if params
        .iter()
        .any(|(k, v)| k == "VALUE" && v.eq_ignore_ascii_case("text"))
    {
        // A free-text birthday ("sometime in spring") isn't a date we can
        // reconstruct — skip rather than guess.
        return None;
    }
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("--") {
        let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() != 4 {
            return None;
        }
        let month: u32 = digits[0..2].parse().ok()?;
        let day: u32 = digits[2..4].parse().ok()?;
        // Placeholder year — the caller treats this pair as "no year".
        let date = NaiveDate::from_ymd_opt(2000, month, day)?;
        return Some((date, false));
    }
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 8 {
        let date = NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()?;
        return Some((date, true));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use jmap_client::jscontact::{Anniversary, AnniversaryDate, EmailAddress, NameProperty};
    use std::collections::BTreeMap;

    #[test]
    fn to_vcard_writes_name_email_and_full_birthday() {
        let mut card = Card::new("uid1");
        card.name = Some(NameProperty {
            full: Some("Ada Lovelace".to_string()),
            ..Default::default()
        });
        let mut emails = BTreeMap::new();
        emails.insert(
            "e1".to_string(),
            EmailAddress {
                address: "ada@example.com".to_string(),
                ..Default::default()
            },
        );
        card.emails = Some(emails);
        let mut anniversaries = BTreeMap::new();
        anniversaries.insert(
            "a1".to_string(),
            Anniversary {
                type_: "Anniversary".to_string(),
                kind: Some("birth".to_string()),
                date: AnniversaryDate::Timestamp {
                    utc: "1815-12-10T00:00:00Z".to_string(),
                },
                extra: BTreeMap::new(),
            },
        );
        card.anniversaries = Some(anniversaries);

        let vcf = to_vcard(&card);
        assert!(vcf.starts_with("BEGIN:VCARD\r\n"));
        assert!(vcf.contains("FN:Ada Lovelace"));
        assert!(vcf.contains("EMAIL:ada@example.com"));
        assert!(vcf.contains("BDAY:1815-12-10"));
        assert!(vcf.contains("UID:uid1"));
        assert!(vcf.ends_with("END:VCARD\r\n"));
    }

    #[test]
    fn to_vcard_round_trips_through_parse_vcards() {
        let mut card = Card::new("uid2");
        card.name = Some(NameProperty {
            full: Some("Grace Hopper".to_string()),
            ..Default::default()
        });
        let mut anniversaries = BTreeMap::new();
        anniversaries.insert(
            "a1".to_string(),
            Anniversary {
                type_: "Anniversary".to_string(),
                kind: Some("birth".to_string()),
                date: AnniversaryDate::PartialDate {
                    year: None,
                    month: Some(7),
                    day: Some(20),
                },
                extra: BTreeMap::new(),
            },
        );
        card.anniversaries = Some(anniversaries);

        let vcf = to_vcard(&card);
        let parsed = parse_vcards(&vcf);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "Grace Hopper");
        let (date, has_year) = parsed[0].birthday.unwrap();
        assert_eq!(date.month(), 7);
        assert_eq!(date.day(), 20);
        assert!(!has_year);
    }

    #[test]
    fn to_vcard_escapes_commas_in_name() {
        let mut card = Card::new("uid3");
        card.name = Some(NameProperty {
            full: Some("Doe, Jane".to_string()),
            ..Default::default()
        });
        let vcf = to_vcard(&card);
        assert!(vcf.contains("FN:Doe\\, Jane"));
    }

    #[test]
    fn parses_fn_email_and_full_birthday() {
        let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nEMAIL;TYPE=pref:ada@example.com\r\nBDAY:1815-12-10\r\nEND:VCARD\r\n";
        let contacts = parse_vcards(vcf);
        assert_eq!(contacts.len(), 1);
        let c = &contacts[0];
        assert_eq!(c.name, "Ada Lovelace");
        assert_eq!(c.email.as_deref(), Some("ada@example.com"));
        let (date, has_year) = c.birthday.unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(1815, 12, 10).unwrap());
        assert!(has_year);
    }

    #[test]
    fn falls_back_to_n_when_fn_missing() {
        let vcf = "BEGIN:VCARD\r\nN:Turing;Alan;;;\r\nEND:VCARD\r\n";
        let contacts = parse_vcards(vcf);
        assert_eq!(contacts[0].name, "Alan Turing");
    }

    #[test]
    fn parses_yearless_birthday() {
        let vcf = "BEGIN:VCARD\r\nFN:Grace Hopper\r\nBDAY:--0720\r\nEND:VCARD\r\n";
        let contacts = parse_vcards(vcf);
        let (date, has_year) = contacts[0].birthday.unwrap();
        assert_eq!(date.month(), 7);
        assert_eq!(date.day(), 20);
        assert!(!has_year);
    }

    #[test]
    fn parses_multiple_cards_and_skips_nameless() {
        let vcf = "BEGIN:VCARD\r\nFN:First Person\r\nEND:VCARD\r\nBEGIN:VCARD\r\nEMAIL:noname@example.com\r\nEND:VCARD\r\nBEGIN:VCARD\r\nFN:Second Person\r\nEND:VCARD\r\n";
        let contacts = parse_vcards(vcf);
        assert_eq!(contacts.len(), 2);
        assert_eq!(contacts[0].name, "First Person");
        assert_eq!(contacts[1].name, "Second Person");
    }
}
