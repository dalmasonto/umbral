//! The swappable `DatabaseRouter` trait and its default implementation.
//! See `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`.

/// A database alias — the key under which a pool is registered
/// (`App::builder().database(alias, pool)`), e.g. `"default"`, `"replica"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Alias(String);

impl Alias {
    pub fn new(s: impl Into<String>) -> Self {
        Alias(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The conventional default alias.
    pub fn default_alias() -> Self {
        Alias("default".to_string())
    }
}

impl From<&str> for Alias {
    fn from(s: &str) -> Self {
        Alias(s.to_string())
    }
}
impl From<String> for Alias {
    fn from(s: String) -> Self {
        Alias(s)
    }
}

/// A validated Postgres schema identifier. Constructed only through
/// [`Schema::new`], which rejects anything that isn't a safe identifier,
/// so a schema name can never be a SQL-injection vector — it is always
/// emitted as a quoted identifier regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema(String);

impl Schema {
    /// Validate and wrap a schema name: `^[A-Za-z_][A-Za-z0-9_]*$`, 1..=63 chars
    /// (Postgres identifier limit). Returns `None` for anything else.
    pub fn new(s: impl Into<String>) -> Option<Self> {
        let s = s.into();
        let ok = (1..=63).contains(&s.len())
            && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        ok.then_some(Schema(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_accepts_valid_identifiers_and_rejects_the_rest() {
        assert!(Schema::new("tenant_7").is_some());
        assert!(Schema::new("_private").is_some());
        assert!(Schema::new("public").is_some());
        // rejects injection / malformed
        assert!(Schema::new("").is_none());
        assert!(Schema::new("1tenant").is_none());
        assert!(Schema::new("a b").is_none());
        assert!(Schema::new("drop\";--").is_none());
        assert!(Schema::new("a".repeat(64)).is_none());
    }

    #[test]
    fn alias_roundtrips() {
        assert_eq!(Alias::from("replica").as_str(), "replica");
        assert_eq!(Alias::default_alias().as_str(), "default");
    }
}
