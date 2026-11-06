/// A typed NFL-player reference. Wraps nfl-data's `String` gsis ids at the
/// boundary; its eventual home is nfl-data next to `TeamAbbr`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, sqlx::Type)]
#[sqlx(transparent)]
pub struct NflPlayerId(pub String);

impl AsRef<str> for NflPlayerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
