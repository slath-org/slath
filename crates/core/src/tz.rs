// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Time zones from the IANA database bundled into the binary (updated with
//! each release), never the host's: every server shows the same instant
//! the same way. Stored times are UTC; these turn one into the wall-clock
//! time of a zone, correct across daylight saving changes and historical
//! offset changes.

use std::sync::OnceLock;

use jiff::tz::{TimeZone, TimeZoneDatabase};

fn db() -> &'static TimeZoneDatabase {
    static DB: OnceLock<TimeZoneDatabase> = OnceLock::new();
    DB.get_or_init(TimeZoneDatabase::bundled)
}

/// The version of the bundled database, such as `2026e`.
#[must_use]
pub fn database_version() -> Option<&'static str> {
    jiff_tzdb::VERSION
}

/// A time zone: UTC or one named in the IANA database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    name: String,
    tz: TimeZone,
}

impl Default for Zone {
    fn default() -> Self {
        Self::utc()
    }
}

/// A moment's wall-clock reading in a zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: i16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    /// The offset from UTC in seconds, east positive.
    pub offset_seconds: i32,
    abbreviation: [u8; 8],
    abbreviation_len: u8,
}

impl Zone {
    #[must_use]
    pub fn utc() -> Self {
        Self {
            name: "UTC".to_owned(),
            tz: TimeZone::UTC,
        }
    }

    /// The zone named `name` (`Europe/Berlin`, `UTC`), if the bundled
    /// database has it. Names are matched as the database spells them.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        if name == "UTC" {
            return Some(Self::utc());
        }
        let tz = db().get(name).ok()?;
        let canonical = tz.iana_name()?;
        (canonical == name).then(|| Self {
            name: name.to_owned(),
            tz,
        })
    }

    /// The zone's IANA name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The wall-clock reading of the moment `unix_seconds` here.
    #[must_use]
    pub fn local(&self, unix_seconds: i64) -> Local {
        let ts = jiff::Timestamp::from_second(unix_seconds).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        let info = self.tz.to_offset_info(ts);
        let dt = info.offset().to_datetime(ts);
        let abbr = info.abbreviation().as_bytes();
        let mut abbreviation = [0u8; 8];
        let n = abbr.len().min(8);
        abbreviation[..n].copy_from_slice(&abbr[..n]);
        Local {
            year: dt.year(),
            month: u8::try_from(dt.month()).unwrap_or(1),
            day: u8::try_from(dt.day()).unwrap_or(1),
            hour: u8::try_from(dt.hour()).unwrap_or(0),
            minute: u8::try_from(dt.minute()).unwrap_or(0),
            offset_seconds: info.offset().seconds(),
            abbreviation,
            #[allow(clippy::cast_possible_truncation)]
            abbreviation_len: n as u8,
        }
    }
}

impl Local {
    /// The zone's name for the reading: its abbreviation (`CET`, `EDT`),
    /// or the offset (`UTC+05:45`) where the database has only a number.
    #[must_use]
    pub fn zone_label(&self) -> String {
        let abbr = std::str::from_utf8(&self.abbreviation[..usize::from(self.abbreviation_len)])
            .unwrap_or("");
        if !abbr.is_empty() && abbr.bytes().all(|b| b.is_ascii_alphabetic()) {
            return abbr.to_owned();
        }
        let s = self.offset_seconds.unsigned_abs();
        let sign = if self.offset_seconds < 0 { '-' } else { '+' };
        format!("UTC{sign}{:02}:{:02}", s / 3600, s % 3600 / 60)
    }

    /// The exact reading: `2026-11-14 10:05 CET`.
    #[must_use]
    pub fn exact(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02} {}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.zone_label()
        )
    }

    /// The calendar date as days since 1970-01-01 (for comparing days).
    #[must_use]
    pub fn day_number(&self) -> i64 {
        jiff::civil::date(
            self.year,
            i8::try_from(self.month).unwrap_or(1),
            i8::try_from(self.day).unwrap_or(1),
        )
        .since(jiff::civil::date(1970, 1, 1))
        .map_or(0, |s| i64::from(s.get_days()))
    }
}

/// Every zone name the bundled database knows, sorted, `UTC` first.
#[must_use]
pub fn names() -> Vec<String> {
    let mut v: Vec<String> = db()
        .available()
        .map(|n| n.as_str().to_owned())
        .filter(|n| n != "UTC" && Zone::named(n).is_some())
        .collect();
    v.sort();
    v.insert(0, "UTC".to_owned());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix seconds of a UTC date and time.
    fn at(y: i16, mo: i8, d: i8, h: i8, mi: i8) -> i64 {
        jiff::civil::date(y, mo, d)
            .at(h, mi, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second()
    }

    fn exact(zone: &str, t: i64) -> String {
        Zone::named(zone).unwrap().local(t).exact()
    }

    #[test]
    fn utc_is_the_default_and_reads_as_before() {
        assert_eq!(Zone::default().name(), "UTC");
        assert_eq!(exact("UTC", at(2026, 11, 14, 9, 5)), "2026-11-14 09:05 UTC");
    }

    #[test]
    fn berlin_across_both_transitions() {
        // Spring forward at 01:00 UTC: 02:00 local becomes 03:00.
        assert_eq!(
            exact("Europe/Berlin", at(2026, 3, 29, 0, 59)),
            "2026-03-29 01:59 CET"
        );
        assert_eq!(
            exact("Europe/Berlin", at(2026, 3, 29, 1, 0)),
            "2026-03-29 03:00 CEST"
        );
        // Fall back: 02:30 local happens twice.
        assert_eq!(
            exact("Europe/Berlin", at(2026, 10, 25, 0, 30)),
            "2026-10-25 02:30 CEST"
        );
        assert_eq!(
            exact("Europe/Berlin", at(2026, 10, 25, 1, 30)),
            "2026-10-25 02:30 CET"
        );
    }

    #[test]
    fn new_york_across_both_transitions() {
        assert_eq!(
            exact("America/New_York", at(2026, 3, 8, 6, 59)),
            "2026-03-08 01:59 EST"
        );
        assert_eq!(
            exact("America/New_York", at(2026, 3, 8, 7, 0)),
            "2026-03-08 03:00 EDT"
        );
        assert_eq!(
            exact("America/New_York", at(2026, 11, 1, 5, 30)),
            "2026-11-01 01:30 EDT"
        );
        assert_eq!(
            exact("America/New_York", at(2026, 11, 1, 6, 30)),
            "2026-11-01 01:30 EST"
        );
    }

    #[test]
    fn lord_howe_moves_by_half_an_hour() {
        // +10:30 in winter, +11 in summer; the database has only numbers.
        assert_eq!(
            exact("Australia/Lord_Howe", at(2026, 7, 1, 0, 0)),
            "2026-07-01 10:30 UTC+10:30"
        );
        assert_eq!(
            exact("Australia/Lord_Howe", at(2026, 1, 1, 0, 0)),
            "2026-01-01 11:00 UTC+11:00"
        );
        // Into summer time: 02:00 local becomes 02:30 (first Sunday of October).
        assert_eq!(
            exact("Australia/Lord_Howe", at(2026, 10, 3, 15, 29)),
            "2026-10-04 01:59 UTC+10:30"
        );
        assert_eq!(
            exact("Australia/Lord_Howe", at(2026, 10, 3, 15, 30)),
            "2026-10-04 02:30 UTC+11:00"
        );
        // Back: 02:00 local becomes 01:30 (first Sunday of April).
        assert_eq!(
            exact("Australia/Lord_Howe", at(2026, 4, 4, 14, 59)),
            "2026-04-05 01:59 UTC+11:00"
        );
        assert_eq!(
            exact("Australia/Lord_Howe", at(2026, 4, 4, 15, 0)),
            "2026-04-05 01:30 UTC+10:30"
        );
    }

    #[test]
    fn kathmandu_and_a_historical_offset_change() {
        assert_eq!(
            exact("Asia/Kathmandu", at(2026, 11, 14, 9, 5)),
            "2026-11-14 14:50 UTC+05:45"
        );
        // Nepal was +05:30 until 1986.
        assert_eq!(
            exact("Asia/Kathmandu", at(1985, 6, 1, 0, 0)),
            "1985-06-01 05:30 UTC+05:30"
        );
        // Samoa skipped 30 December 2011, moving from -10 to +14 (then +13).
        assert_eq!(
            exact("Pacific/Apia", at(2011, 12, 30, 9, 59)),
            "2011-12-29 23:59 UTC-10:00"
        );
        assert_eq!(
            exact("Pacific/Apia", at(2011, 12, 30, 10, 0)),
            "2011-12-31 00:00 UTC+14:00"
        );
    }

    #[test]
    fn names_are_the_databases_and_nothing_else() {
        let n = names();
        assert_eq!(n[0], "UTC");
        assert!(n.len() > 300, "{}", n.len());
        for z in [
            "Europe/Berlin",
            "America/New_York",
            "Asia/Kathmandu",
            "Pacific/Kiritimati",
            "Etc/GMT+12",
        ] {
            assert!(n.iter().any(|x| x == z), "{z}");
            assert!(Zone::named(z).is_some(), "{z}");
        }
        for bad in [
            "",
            "europe/berlin",
            "Mars/Olympus",
            "../../etc/passwd",
            "Europe/Berlin ",
        ] {
            assert!(Zone::named(bad).is_none(), "{bad:?}");
        }
        assert!(database_version().is_some());
    }

    #[test]
    fn day_numbers_compare_calendar_days() {
        let t = at(2026, 11, 14, 11, 0);
        let kiritimati = Zone::named("Pacific/Kiritimati").unwrap().local(t);
        let baker = Zone::named("Etc/GMT+12").unwrap().local(t);
        assert_eq!((kiritimati.day, baker.day), (15, 13));
        assert_eq!(kiritimati.day_number() - baker.day_number(), 2);
    }
}
