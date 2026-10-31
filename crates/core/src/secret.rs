// SPDX-FileCopyrightText: 2026 Erich Seifert
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A wrapper that keeps secrets out of logs.

use std::fmt;

/// Holds a password, token, or key. `Debug` and `Display` never print the
/// value; call [`Secret::expose`] at the single place that needs it.
///
/// Deliberately implements neither `Serialize` nor `Clone`, so a secret can't
/// end up in a response or log record by accident.
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Returns the wrapped value. Keep the result out of logs and errors.
    pub const fn expose(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_redact() {
        let s = Secret::new("x");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(!format!("{s:?}").contains('x'));
        assert!(!format!("{s}").contains('x'));
        assert!(!format!("{:#?}", Some(Secret::new("hunter2"))).contains("hunter2"));
        assert_eq!(*s.expose(), "x");
    }
}
