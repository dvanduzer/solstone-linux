// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use gtk::glib;
use num_format::{Buffer, Locale, SystemLocale, ToFormattedStr};

pub fn format_integer<T: ToFormattedStr>(n: T) -> String {
    if let Ok(locale) = SystemLocale::default() {
        let mut buf = Buffer::default();
        buf.write_formatted(&n, &locale);
        buf.to_string()
    } else {
        let mut buf = Buffer::default();
        buf.write_formatted(&n, &Locale::en);
        buf.to_string()
    }
}

pub fn format_timestamp(epoch_secs: f64) -> String {
    if let Ok(dt) = glib::DateTime::from_unix_local(epoch_secs as i64)
        && let Ok(s) = dt.format("%Y-%m-%d %H:%M:%S")
    {
        return s.to_string();
    }
    format!("{epoch_secs:.0}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_integer_fallback_does_not_panic() {
        let s1 = format_integer(1000);
        assert!(!s1.is_empty());
        let s2 = format_integer(0);
        assert_eq!(s2, "0");
        let s3 = format_integer(-42);
        assert!(s3.contains("42"));
    }

    #[test]
    fn format_timestamp_does_not_panic() {
        let s = format_timestamp(1700000000.0);
        assert!(!s.is_empty());
    }
}
