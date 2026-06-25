//! MultiChoice: closed-set multi-valued field support.
//!
//! Where a [`ChoiceField`](crate::orm::ChoiceField) field carries a single
//! enum variant, a `MultiChoice<E>` field carries an ordered list of
//! distinct variants of the same enum. Storage is a single TEXT column
//! holding a comma-separated list of the variants' DB values
//! (e.g. `"design,frontend"`); decoding is total against `E`'s
//! `from_str_ok`, so a stray value in the column fails fast at the sqlx
//! decode boundary.
//!
//! ```ignore
//! use umbral::prelude::*;
//!
//! #[derive(Debug, Clone, Copy, PartialEq, Eq, Choices)]
//! #[choices(rename_all = "lowercase")]
//! pub enum Tag { Design, Frontend, Backend, DevOps }
//!
//! #[derive(Debug, Clone, serde::Serialize, sqlx::FromRow, Model)]
//! pub struct Article {
//!     pub id: i64,
//!     pub title: String,
//!     #[umbral(default = "design,frontend")]
//!     pub tags: MultiChoice<Tag>,
//! }
//! ```
//!
//! The admin renders the field as a checkbox-chip group (one chip per
//! variant) backed by a hidden CSV input. The Postgres / SQLite
//! migration emits a plain `TEXT` column — no CHECK constraint at v1,
//! since validating "every CSV piece is a known variant" requires a
//! regex which we'd have to escape per-variant. Application-layer
//! enforcement via sqlx's `Decode` path is sufficient.

use crate::orm::ChoiceField;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::{Database, Decode, Encode, Postgres, Sqlite, Type};
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

/// An ordered list of [`ChoiceField`] variants, persisted as a single
/// comma-separated TEXT column. The Rust type is the structural
/// constraint: every element is statically `E`, so the only way an
/// invalid value can land in the database is by a third-party process
/// writing directly. sqlx's `Decode` then fails the next read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MultiChoice<E: ChoiceField> {
    values: Vec<E>,
}

impl<E: ChoiceField> MultiChoice<E> {
    /// An empty selection.
    pub const fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Construct from a `Vec<E>`. Duplicates are NOT removed; callers
    /// that need a set should dedup their input.
    pub fn from_vec(values: Vec<E>) -> Self {
        Self { values }
    }

    /// Borrow the underlying slice.
    pub fn as_slice(&self) -> &[E] {
        &self.values
    }

    /// Take the underlying `Vec<E>`.
    pub fn into_vec(self) -> Vec<E> {
        self.values
    }

    /// Number of selected variants.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// True when no variants are selected.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Append a variant. No de-duplication.
    pub fn push(&mut self, value: E) {
        self.values.push(value);
    }

    /// True when `value` appears at least once.
    pub fn contains(&self, value: &E) -> bool
    where
        E: PartialEq,
    {
        self.values.contains(value)
    }

    /// The DB-stored TEXT value for the current selection. Empty string
    /// for an empty selection.
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        for (i, v) in self.values.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(v.as_str());
        }
        out
    }

    /// Parse a comma-separated DB string into a `MultiChoice<E>`.
    /// Empty input yields an empty selection. Whitespace around
    /// individual entries is trimmed. Unknown segments return
    /// `Err(unknown_segment)`.
    pub fn from_csv(s: &str) -> Result<Self, String> {
        if s.is_empty() {
            return Ok(Self::new());
        }
        let mut values = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match E::from_str_ok(part) {
                Some(v) => values.push(v),
                None => return Err(part.to_string()),
            }
        }
        Ok(Self { values })
    }
}

impl<E: ChoiceField> From<Vec<E>> for MultiChoice<E> {
    fn from(values: Vec<E>) -> Self {
        Self::from_vec(values)
    }
}

impl<E: ChoiceField> FromIterator<E> for MultiChoice<E> {
    fn from_iter<I: IntoIterator<Item = E>>(iter: I) -> Self {
        Self {
            values: iter.into_iter().collect(),
        }
    }
}

impl<E: ChoiceField> IntoIterator for MultiChoice<E> {
    type Item = E;
    type IntoIter = std::vec::IntoIter<E>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}

impl<'a, E: ChoiceField> IntoIterator for &'a MultiChoice<E> {
    type Item = &'a E;
    type IntoIter = std::slice::Iter<'a, E>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl<E: ChoiceField> std::ops::Deref for MultiChoice<E> {
    type Target = [E];
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<E: ChoiceField> fmt::Display for MultiChoice<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_csv())
    }
}

impl<E: ChoiceField> FromStr for MultiChoice<E> {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_csv(s)
    }
}

// =========================================================================
// serde
// =========================================================================

/// On the wire (REST, admin JSON, fixtures) a `MultiChoice<E>` is the
/// natural JSON array of strings — `["design", "frontend"]` — not the
/// CSV form used at the DB layer. The two storage shapes are
/// deliberately separate: the CSV form is an implementation detail of
/// the TEXT column, and external consumers never see it.
impl<E: ChoiceField> Serialize for MultiChoice<E> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.values.len()))?;
        for v in &self.values {
            seq.serialize_element(v.as_str())?;
        }
        seq.end()
    }
}

/// Deserialize accepts both the natural JSON array form
/// (`["design","frontend"]`) and the CSV string form
/// (`"design,frontend"`). The latter is what HTML form posts produce
/// when the admin's hidden CSV input round-trips through
/// `application/x-www-form-urlencoded`.
impl<'de, E: ChoiceField> Deserialize<'de> for MultiChoice<E> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct V<E>(PhantomData<E>);
        impl<'de, E: ChoiceField> serde::de::Visitor<'de> for V<E> {
            type Value = MultiChoice<E>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a CSV string or a JSON array of choice strings")
            }
            fn visit_str<X: serde::de::Error>(self, s: &str) -> Result<Self::Value, X> {
                MultiChoice::from_csv(s)
                    .map_err(|bad| X::custom(format!("unknown MultiChoice variant `{bad}`")))
            }
            fn visit_string<X: serde::de::Error>(self, s: String) -> Result<Self::Value, X> {
                self.visit_str(&s)
            }
            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut values: Vec<E> = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    match E::from_str_ok(&s) {
                        Some(v) => values.push(v),
                        None => {
                            return Err(serde::de::Error::custom(format!(
                                "unknown MultiChoice variant `{s}`"
                            )));
                        }
                    }
                }
                Ok(MultiChoice { values })
            }
        }
        deserializer.deserialize_any(V::<E>(PhantomData))
    }
}

// =========================================================================
// sqlx — TEXT column on both backends
// =========================================================================

impl<E: ChoiceField, DB: Database> Type<DB> for MultiChoice<E>
where
    String: Type<DB>,
{
    fn type_info() -> DB::TypeInfo {
        <String as Type<DB>>::type_info()
    }
    fn compatible(ty: &DB::TypeInfo) -> bool {
        <String as Type<DB>>::compatible(ty)
    }
}

impl<'q, E: ChoiceField> Encode<'q, Sqlite> for MultiChoice<E> {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let csv = self.to_csv();
        <String as Encode<'q, Sqlite>>::encode(csv, buf)
    }
}

impl<'r, E: ChoiceField> Decode<'r, Sqlite> for MultiChoice<E> {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as Decode<'r, Sqlite>>::decode(value)?;
        MultiChoice::<E>::from_csv(&s).map_err(|bad| {
            format!(
                "unknown MultiChoice<{}> variant `{bad}`",
                std::any::type_name::<E>()
            )
            .into()
        })
    }
}

impl<'q, E: ChoiceField> Encode<'q, Postgres> for MultiChoice<E> {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let csv = self.to_csv();
        <String as Encode<'q, Postgres>>::encode(csv, buf)
    }
}

impl<'r, E: ChoiceField> Decode<'r, Postgres> for MultiChoice<E> {
    fn decode(
        value: <Postgres as Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as Decode<'r, Postgres>>::decode(value)?;
        MultiChoice::<E>::from_csv(&s).map_err(|bad| {
            format!(
                "unknown MultiChoice<{}> variant `{bad}`",
                std::any::type_name::<E>()
            )
            .into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::ChoiceField;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Tag {
        Design,
        Frontend,
        Backend,
    }

    impl ChoiceField for Tag {
        const VALUES: &'static [&'static str] = &["design", "frontend", "backend"];
        const LABELS: &'static [&'static str] = &["Design", "Frontend", "Backend"];
        fn as_str(&self) -> &'static str {
            match self {
                Tag::Design => "design",
                Tag::Frontend => "frontend",
                Tag::Backend => "backend",
            }
        }
        fn from_str_ok(s: &str) -> Option<Self> {
            match s {
                "design" => Some(Tag::Design),
                "frontend" => Some(Tag::Frontend),
                "backend" => Some(Tag::Backend),
                _ => None,
            }
        }
    }

    #[test]
    fn csv_roundtrip() {
        let mc: MultiChoice<Tag> = vec![Tag::Design, Tag::Backend].into();
        assert_eq!(mc.to_csv(), "design,backend");
        let parsed: MultiChoice<Tag> = MultiChoice::from_csv("design,backend").unwrap();
        assert_eq!(parsed, mc);
    }

    #[test]
    fn empty_csv_is_empty_selection() {
        let mc: MultiChoice<Tag> = MultiChoice::from_csv("").unwrap();
        assert!(mc.is_empty());
        assert_eq!(mc.to_csv(), "");
    }

    #[test]
    fn csv_trims_whitespace_and_skips_blanks() {
        let mc: MultiChoice<Tag> = MultiChoice::from_csv(" design , , backend ").unwrap();
        assert_eq!(mc.as_slice(), &[Tag::Design, Tag::Backend]);
    }

    #[test]
    fn csv_rejects_unknown_variant() {
        let err = MultiChoice::<Tag>::from_csv("design,bogus").unwrap_err();
        assert_eq!(err, "bogus");
    }

    #[test]
    fn serde_emits_json_array() {
        let mc: MultiChoice<Tag> = vec![Tag::Design, Tag::Frontend].into();
        let json = serde_json::to_string(&mc).unwrap();
        assert_eq!(json, r#"["design","frontend"]"#);
    }

    #[test]
    fn serde_accepts_json_array() {
        let mc: MultiChoice<Tag> = serde_json::from_str(r#"["design","backend"]"#).unwrap();
        assert_eq!(mc.as_slice(), &[Tag::Design, Tag::Backend]);
    }

    #[test]
    fn serde_accepts_csv_string() {
        let mc: MultiChoice<Tag> = serde_json::from_str(r#""design,backend""#).unwrap();
        assert_eq!(mc.as_slice(), &[Tag::Design, Tag::Backend]);
    }

    #[test]
    fn deref_to_slice() {
        let mc: MultiChoice<Tag> = vec![Tag::Design].into();
        let s: &[Tag] = &mc;
        assert_eq!(s, &[Tag::Design]);
    }
}
