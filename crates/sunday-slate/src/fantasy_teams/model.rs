use crate::media::Media;
use crate::media::MediaId;
use time::PrimitiveDateTime;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FantasyTeam {
    pub id: i64,
    pub league_id: i64,
    pub user_id: i64,
    pub name: String,
    pub owner_name: String,
    pub logo_media_id: Option<MediaId>,
    pub is_commissioner: bool,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

/// A league's fantasy team with its logo joined in — the roster listing,
/// without the columns only the team's own pages need.
pub struct LeagueTeam {
    pub id: FantasyTeamId,
    pub name: String,
    pub owner_name: String,
    pub logo: Option<Media>,
}

/// A fantasy team's id as an opaque token: typed so it cannot be confused
/// with other ids, open because the type distinction is the protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct FantasyTeamId(pub i64);

/// Up to two initials from a fantasy team name, for the logo fallback.
pub fn monogram(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monogram_takes_up_to_two_initials() {
        assert_eq!(monogram("Gridiron Giants"), "GG");
        assert_eq!(monogram("Solo"), "S");
        assert_eq!(monogram("a b c"), "AB");
        assert_eq!(monogram(""), "");
    }
}
