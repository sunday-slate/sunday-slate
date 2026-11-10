use std::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

use sqlx::{Database, Decode, Encode, Type, encode::IsNull, error::BoxDynError};

use crate::{Season, SeasonType, Week};

/// An NFL week identified by season year and absolute week number.
///
/// Weeks 1–18 are regular season and weeks 19–22 are postseason, so a week's classification is
/// derived from its number rather than stored alongside it.
///
/// Equality, ordering, hashing, and SQLx integer storage use the value returned by
/// [`encode`](Self::encode).
///
/// # Examples
///
/// ```
/// use nfl_model::{Season, SeasonWeek, Week};
///
/// let week = SeasonWeek {
///     season: Season(2026),
///     week: Week(1),
/// };
/// assert_eq!(week.encode(), 2_026_001);
/// assert_eq!(SeasonWeek::decode(2_026_001), Some(week));
/// assert!(!week.postseason());
/// ```
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SeasonWeek {
    /// Year identifying the NFL season, including for postseason games in the next calendar year.
    pub season: Season,
    /// Absolute week number: 1–18 for the regular season, 19–22 for the postseason.
    pub week: Week,
}

impl SeasonWeek {
    /// Create a validated week from a season and an absolute week number.
    ///
    /// Accepts weeks 1–22, where 1–18 are regular season and 19–22 are postseason (wild card,
    /// divisional, championship, and Super Bowl). Returns `None` for 0 and for anything above 22.
    pub fn new(season: Season, week: Week) -> Option<Self> {
        match week.0 {
            1..=22 => Some(Self { season, week }),
            _ => None,
        }
    }

    /// Create a validated week in the default [`Season`].
    ///
    /// Accepts weeks 1–22 like [`new`](Self::new); use `new` to pick the season explicitly.
    pub fn from_week(week: Week) -> Option<Self> {
        Self::new(Season::default(), week)
    }

    /// Whether this week is a postseason week (week 19 or later).
    pub fn postseason(&self) -> bool {
        self.week.0 > 18
    }

    /// Enumerate the weeks of a season in schedule-independent order: weeks 1–22.
    ///
    /// Postseason weeks keep the supplied season year even when played in the next calendar
    /// year. Order reflects this canonical enumeration, not the NFL schedule.
    pub fn for_season(season: Season) -> impl Iterator<Item = Self> {
        (1..=22).map(move |week| Self {
            season,
            week: Week(week),
        })
    }

    /// Encode as `season * 1_000 + postseason * 100 + week`.
    ///
    /// The classification digit is `0` for regular-season weeks and `1` for postseason weeks.
    pub fn encode(&self) -> u32 {
        u32::from(self.season.0) * 1_000
            + u32::from(self.postseason()) * 100
            + u32::from(self.week.0)
    }

    /// Decode a value in the integer format produced by [`encode`](Self::encode).
    ///
    /// Returns `None` if the classification digit is neither `0` nor `1` or the week is not a
    /// valid week for that classification. Stored postseason aliases 1–3 normalize to canonical
    /// weeks 19–21.
    pub fn decode(value: u32) -> Option<Self> {
        let year = (value / 1_000) as u16;
        let choice = ((value / 100) % 10) as u8;
        let week = (value % 100) as u8;

        let season_type = SeasonType::try_from(choice).ok()?;

        let week = match (season_type, week) {
            (SeasonType::Reg, 1..=18) => Week(week),
            (SeasonType::Post, 1..=3) => Week(week + 18),
            (SeasonType::Post, 19..=22) => Week(week),
            _ => return None,
        };

        Self::new(Season(year), week)
    }
}

impl PartialEq for SeasonWeek {
    fn eq(&self, other: &Self) -> bool {
        self.encode() == other.encode()
    }
}

impl Eq for SeasonWeek {}

impl Hash for SeasonWeek {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.encode().hash(state);
    }
}

impl PartialOrd for SeasonWeek {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SeasonWeek {
    fn cmp(&self, other: &Self) -> Ordering {
        self.encode().cmp(&other.encode())
    }
}

// Make SeasonWeek encode/decode to an i64 for SQLx (sqlite) storage in an integer

impl<DB> Type<DB> for SeasonWeek
where
    DB: Database,
    i64: Type<DB>,
{
    fn type_info() -> DB::TypeInfo {
        <i64 as Type<DB>>::type_info()
    }

    fn compatible(ty: &DB::TypeInfo) -> bool {
        <i64 as Type<DB>>::compatible(ty)
    }
}

impl<'q, DB> Encode<'q, DB> for SeasonWeek
where
    DB: Database,
    i64: Encode<'q, DB>,
{
    fn encode_by_ref(&self, buf: &mut DB::ArgumentBuffer) -> Result<IsNull, BoxDynError> {
        let encoded = i64::from(self.encode());
        <i64 as Encode<DB>>::encode_by_ref(&encoded, buf)
    }
}

impl<'r, DB> Decode<'r, DB> for SeasonWeek
where
    DB: Database,
    i64: Decode<'r, DB>,
{
    fn decode(value: DB::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let encoded = <i64 as Decode<DB>>::decode(value)?;
        let encoded = u32::try_from(encoded)?;

        SeasonWeek::decode(encoded).ok_or_else(|| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid encoded SeasonWeek: {encoded}"),
            )) as BoxDynError
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SeasonWeek;
    use crate::{Season, Week};
    use std::collections::HashSet;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn week(season: u16, week: u8) -> SeasonWeek {
        SeasonWeek::new(Season(season), Week(week)).expect("valid season week")
    }

    #[test]
    fn new_accepts_absolute_weeks_one_through_twenty_two() {
        for (input, expected, postseason) in [
            (0u8, None, false),
            (1, Some(1), false),
            (18, Some(18), false),
            (19, Some(19), true),
            (22, Some(22), true),
            (23, None, false),
            (255, None, false),
        ] {
            let created = SeasonWeek::new(Season(2026), Week(input));

            match expected {
                Some(expected) => {
                    let created = created.expect("valid week");
                    assert_eq!(created.season.0, 2026);
                    assert_eq!(created.week.0, expected);
                    assert_eq!(created.postseason(), postseason);
                }
                None => assert_eq!(created, None, "week {input} must be rejected"),
            }
        }
    }

    #[test]
    fn for_season_enumerates_weeks_one_through_twenty_two() {
        let weeks: Vec<_> = SeasonWeek::for_season(Season(2026)).collect();

        assert_eq!(weeks.len(), 22);
        for (index, week) in weeks.iter().enumerate() {
            assert_eq!(week.week.0 as usize, index + 1);
            assert_eq!(week.season.0, 2026);
            assert_eq!(week.postseason(), index + 1 > 18);
        }
    }

    #[test]
    fn encode_and_decode_round_trip() {
        for (encoded, canonical_week) in [(2_026_018u32, 18u8), (2_026_119, 19), (2_026_122, 22)] {
            let decoded = SeasonWeek::decode(encoded).expect("decodable");
            assert_eq!(decoded.season.0, 2026);
            assert_eq!(decoded.week.0, canonical_week);
            assert_eq!(decoded.encode(), encoded);
        }

        assert_eq!(week(2026, 18).encode(), 2_026_018);
        assert_eq!(week(2026, 19).encode(), 2_026_119);
        assert_eq!(week(2026, 22).encode(), 2_026_122);
    }

    #[test]
    fn decode_normalizes_stored_postseason_aliases() {
        for (stored, canonical) in [(2_026_101u32, 19u8), (2_026_102, 20), (2_026_103, 21)] {
            let decoded = SeasonWeek::decode(stored).expect("decodable alias");
            assert_eq!(decoded.week.0, canonical);
            assert_eq!(decoded, week(2026, canonical));
            assert_eq!(decoded.encode(), 2_026_100 + u32::from(canonical));
        }
    }

    #[test]
    fn decode_rejects_invalid_classification_and_week() {
        for encoded in [
            2_026_104u32, // postseason alias above 3
            2_026_019,    // regular season has 18 weeks
            2_026_118,    // postseason week 18 is canonicalized as regular season
            2_026_123,    // week 23
            2_026_201,    // invalid classification digit
        ] {
            assert_eq!(
                SeasonWeek::decode(encoded),
                None,
                "{encoded} must be rejected"
            );
        }
    }

    #[test]
    fn alias_decoded_values_match_constructed_canonical_values() {
        let alias = SeasonWeek::decode(2_026_101).unwrap();
        let canonical = week(2026, 19);

        assert_eq!(alias, canonical);
        assert_eq!(hash(&alias), hash(&canonical));

        let set: HashSet<SeasonWeek> = [alias, canonical].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    fn hash(week: &SeasonWeek) -> u64 {
        let mut hasher = DefaultHasher::new();
        week.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn ordering_follows_chronological_weeks_and_seasons() {
        assert!(week(2026, 18) < week(2026, 19));
        assert!(week(2026, 19) < week(2026, 22));
        assert!(week(2026, 22) < week(2027, 1));
        assert!(SeasonWeek::decode(2_026_101).unwrap() < week(2026, 20));
    }

    #[tokio::test]
    async fn sqlite_bound_query_round_trips_encoded_weeks() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");

        let (stored,) = sqlx::query_as::<_, (SeasonWeek,)>("SELECT ?")
            .bind(week(2026, 22))
            .fetch_one(&pool)
            .await
            .expect("decode valid week");
        assert_eq!(stored, week(2026, 22));
        assert_eq!(stored.encode(), 2_026_122);

        let invalid = sqlx::query_as::<_, (SeasonWeek,)>("SELECT ?")
            .bind(2_026_019i64)
            .fetch_one(&pool)
            .await;
        assert!(invalid.is_err(), "invalid encoded week must fail to decode");

        pool.close().await;
    }
}
