//! Wi-Fi logic: the regulatory country (`sys/net/wifi/country`) and the list
//! to choose it from.
//!
//! The country decides which channels the radio may use and where it may
//! transmit first (docs/architecture/wifi.md): unset, the world domain
//! `00` applies, which only listens on most 5 GHz channels and never uses
//! 12-13 on 2.4 GHz. `wlanmd` re-reads the key every few seconds and gives it
//! to each radio, so a change needs no restart. It is a machine setting, so
//! the write goes through the store's elevation (`elevd`, an administrator
//! approves); a code that is not on the ISO 3166-1 list is refused before
//! that.

use crate::store::{ConfigStore, StoreError, Value};

/// The machine's regulatory country, a two-letter code or `00`.
pub const KEY_COUNTRY: &str = "sys/net/wifi/country";

/// One row of the list: a code and what the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub code: &'static str,
    pub label: String,
}

/// The world domain first, then every country by name.
pub fn entries() -> Vec<Entry> {
    let mut countries: Vec<Entry> = iso3166::COUNTRIES
        .iter()
        .map(|(code, name)| Entry {
            code,
            label: format!("{name} ({code})"),
        })
        .collect();
    countries.sort_by(|a, b| a.label.cmp(&b.label));
    let mut all = vec![Entry {
        code: iso3166::WORLD,
        label: format!("{} ({})", iso3166::WORLD_NAME, iso3166::WORLD),
    }];
    all.extend(countries);
    all
}

/// The label of `code`, for a status line (`None` for an unknown code).
pub fn label_of(code: &str) -> Option<String> {
    let bytes = iso3166::parse(code)?;
    let name = iso3166::display_name(&bytes)?;
    Some(format!("{name} ({})", String::from_utf8_lossy(&bytes)))
}

/// The indexes into `entries` whose label or code contains `query`, ignoring
/// case; every index for an empty query.
pub fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| query.is_empty() || entry.label.to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

/// The stored country as a code, `None` when unset or not a valid code.
pub fn current(store: &dyn ConfigStore) -> Option<String> {
    match store.get(KEY_COUNTRY)? {
        Value::Str(text) => iso3166::parse(&text).map(|code| String::from_utf8_lossy(&code).into()),
        _ => None,
    }
}

/// What the page says is in effect.
pub fn describe_current(store: &dyn ConfigStore) -> String {
    match current(store).and_then(|code| label_of(&code)) {
        Some(label) if label.ends_with("(00)") => {
            String::from("Current country: World (00), the cautious default.")
        }
        Some(label) => format!("Current country: {label}."),
        None => String::from("No country is set: the world domain (00) applies."),
    }
}

/// The country of a time zone, as a suggestion. `None` for UTC and for a
/// zone this table does not know.
pub fn suggestion(zone: &str) -> Option<&'static str> {
    Some(match zone {
        "Europe/London" => "GB",
        "Europe/Paris" => "FR",
        "Europe/Berlin" => "DE",
        "America/New_York" | "America/Chicago" | "America/Los_Angeles" => "US",
        "Asia/Kolkata" => "IN",
        "Asia/Tokyo" => "JP",
        "Australia/Sydney" => "AU",
        _ => return None,
    })
}

/// Store `code` (checked against the list).
pub fn set(store: &dyn ConfigStore, code: &str) -> Result<(), StoreError> {
    let code = iso3166::parse(code)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .ok_or_else(|| String::from("not a country code"))?;
    store.set(KEY_COUNTRY, Value::Str(code))
}

/// Write `code` unless it is already the stored one; the status line text.
pub fn apply(store: &dyn ConfigStore, code: Option<&str>) -> String {
    let Some(code) = code else {
        return String::from("Select a country first.");
    };
    let Some(label) = label_of(code) else {
        return String::from("That is not a country code.");
    };
    if current(store).as_deref() == Some(code) {
        return format!("The country already is {label}.");
    }
    match set(store, code) {
        Ok(()) => {
            println!("SETTINGS:WIFI:COUNTRY:{code}");
            format!("Wi-Fi country set to {label}.")
        }
        Err(error) => format!("The Wi-Fi country was not changed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    fn codes(entries: &[Entry], rows: &[usize]) -> Vec<&'static str> {
        rows.iter().map(|row| entries[*row].code).collect()
    }

    #[test]
    fn the_list_is_the_world_then_every_country_by_name() {
        let all = entries();
        assert_eq!(all.len(), 250);
        assert_eq!(all[0].code, "00");
        assert_eq!(all[0].label, "World (00)");
        let labels: Vec<&str> = all[1..].iter().map(|e| e.label.as_str()).collect();
        assert!(labels.windows(2).all(|w| w[0] <= w[1]), "sorted by name");
        let france = all.iter().find(|e| e.code == "FR").unwrap();
        assert_eq!(france.label, "France (FR)");
        // Every entry parses back as a regulatory code.
        assert!(all.iter().all(|e| iso3166::parse(e.code).is_some()));
    }

    #[test]
    fn typing_narrows_the_list_by_name_or_code() {
        let all = entries();
        assert_eq!(filter(&all, "").len(), 250);
        assert_eq!(filter(&all, "   ").len(), 250);
        assert_eq!(codes(&all, &filter(&all, "france")), ["FR"]);
    }

    #[test]
    fn the_filter_matches_ignoring_case_and_finds_nothing_for_nonsense() {
        let all = entries();
        let hits = codes(&all, &filter(&all, "FrAnCe"));
        assert!(hits.contains(&"FR"), "{hits:?}");
        assert_eq!(codes(&all, &filter(&all, "(fr)")), ["FR"]);
        assert_eq!(codes(&all, &filter(&all, "world")), ["00"]);
        assert!(filter(&all, "qqqq nowhere").is_empty());
    }

    #[test]
    fn labels_come_from_the_same_list() {
        assert_eq!(label_of("US").as_deref(), Some("United States (US)"));
        assert_eq!(label_of("us").as_deref(), Some("United States (US)"));
        assert_eq!(label_of("00").as_deref(), Some("World (00)"));
        assert_eq!(label_of("ZZ"), None);
        assert_eq!(label_of(""), None);
    }

    #[test]
    fn the_stored_country_is_read_back_checked() {
        let store = MemStore::new();
        assert_eq!(current(&store), None);
        assert_eq!(
            describe_current(&store),
            "No country is set: the world domain (00) applies."
        );
        store.set(KEY_COUNTRY, Value::Str("FR".into())).unwrap();
        assert_eq!(current(&store).as_deref(), Some("FR"));
        assert_eq!(describe_current(&store), "Current country: France (FR).");
        store.set(KEY_COUNTRY, Value::Str("00".into())).unwrap();
        assert!(describe_current(&store).starts_with("Current country: World (00)"));
        // A value that is not a country, or of the wrong kind, reads as unset.
        store.set(KEY_COUNTRY, Value::Str("ZZ".into())).unwrap();
        assert_eq!(current(&store), None);
        store.set(KEY_COUNTRY, Value::Bool(true)).unwrap();
        assert_eq!(current(&store), None);
    }

    #[test]
    fn every_zone_is_suggested_a_country_or_is_utc() {
        for zone in timezone::ZONES {
            match suggestion(zone.name) {
                Some(code) => assert!(label_of(code).is_some(), "{}: {code}", zone.name),
                None => assert_eq!(zone.name, "UTC", "{} has no country", zone.name),
            }
        }
        assert_eq!(suggestion("Europe/Paris"), Some("FR"));
        assert_eq!(suggestion("Mars/Olympus"), None);
    }

    #[test]
    fn apply_writes_once_and_only_a_change() {
        let store = MemStore::new();
        assert_eq!(apply(&store, None), "Select a country first.");
        assert!(store.is_empty());
        assert_eq!(
            apply(&store, Some("FR")),
            "Wi-Fi country set to France (FR)."
        );
        assert_eq!(current(&store).as_deref(), Some("FR"));
        // The same country again is not a second request (no second prompt).
        *store.fail_writes.borrow_mut() = Some("would prompt".into());
        assert_eq!(
            apply(&store, Some("FR")),
            "The country already is France (FR)."
        );
    }

    #[test]
    fn a_made_up_code_never_reaches_the_store() {
        let store = MemStore::new();
        assert_eq!(apply(&store, Some("ZZ")), "That is not a country code.");
        assert!(set(&store, "UK").is_err());
        assert!(set(&store, "").is_err());
        assert!(store.is_empty());
        set(&store, "de").unwrap();
        assert_eq!(current(&store).as_deref(), Some("DE"), "stored in capitals");
    }

    #[test]
    fn a_refused_write_keeps_the_stored_country() {
        let store = MemStore::new();
        store.set(KEY_COUNTRY, Value::Str("GB".into())).unwrap();
        *store.fail_writes.borrow_mut() = Some("cancelled".into());
        let text = apply(&store, Some("FR"));
        assert!(text.contains("cancelled"), "{text}");
        assert_eq!(current(&store).as_deref(), Some("GB"));
    }
}
