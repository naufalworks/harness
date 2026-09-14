//! Three-state patch fields for JSON merge-patch style updates.
//!
//! A PATCH body has three distinct meanings for every field: the field is absent ("leave it
//! alone"), present and `null` ("clear it"), or present with a value ("set it"). Spelling that
//! as `Option<Option<T>>` compiles, but nothing about the type says which nesting level means
//! which, so every reader has to rediscover it and a single dropped layer silently turns a
//! clear into a no-op. `Patch<T>` names the three states instead.
//!
//! Wire mapping, given `#[serde(default)]` on the field:
//! - absent  -> `Patch::Unchanged` (via `Default`; `Deserialize` is never called)
//! - `null`  -> `Patch::Clear`
//! - value   -> `Patch::Set(value)`
use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Patch<T> {
    /// The caller did not name this field.
    #[default]
    Unchanged,
    /// The caller sent `null`: store no value.
    Clear,
    /// The caller sent a value: store it.
    Set(T),
}

impl<T> Patch<T> {
    /// True when the caller stated no intent for this field.
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Patch::Unchanged)
    }

    /// The value the caller wants stored, if any. `Clear` and `Unchanged` both have none, so
    /// use this only for validating a supplied value, never to decide whether to write.
    pub fn value(&self) -> Option<&T> {
        match self {
            Patch::Set(value) => Some(value),
            _ => None,
        }
    }

    /// Apply this field to the current stored value. `Unchanged` leaves it exactly as it was;
    /// that is the whole point of the type, so applying is the only supported way to merge.
    pub fn apply(self, target: &mut Option<T>) {
        match self {
            Patch::Unchanged => {}
            Patch::Clear => *target = None,
            Patch::Set(value) => *target = Some(value),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Reached only when the field is present, so `null` is a deliberate clear.
        Ok(match Option::deserialize(deserializer)? {
            Some(value) => Patch::Set(value),
            None => Patch::Clear,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Deserialize)]
    struct Body {
        #[serde(default)]
        field: Patch<String>,
    }

    #[test]
    fn absent_null_and_value_stay_three_different_answers() {
        assert_eq!(
            serde_json::from_str::<Body>("{}").unwrap().field,
            Patch::Unchanged
        );
        assert_eq!(
            serde_json::from_str::<Body>(r#"{"field":null}"#)
                .unwrap()
                .field,
            Patch::Clear
        );
        assert_eq!(
            serde_json::from_str::<Body>(r#"{"field":"x"}"#)
                .unwrap()
                .field,
            Patch::Set("x".to_string())
        );
    }

    #[test]
    fn only_a_named_field_moves_the_stored_value() {
        let mut stored = Some("before".to_string());
        Patch::Unchanged.apply(&mut stored);
        assert_eq!(stored.as_deref(), Some("before"));
        Patch::Set("after".to_string()).apply(&mut stored);
        assert_eq!(stored.as_deref(), Some("after"));
        Patch::<String>::Clear.apply(&mut stored);
        assert_eq!(stored, None);
    }
}
