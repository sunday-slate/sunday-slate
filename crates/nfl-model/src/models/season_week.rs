use std::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

use sqlx::{Database, Decode, Encode, Type, encode::IsNull, error::BoxDynError};

use crate::{Season, SeasonType, Week};

/// An NFL week identified by season year, season type, and week number.
///
/// Regular-season and postseason weeks are distinct even when their week numbers match.
/// [`reg`](Self::reg) and [`post`](Self::post) use the default [`Season`]; construct the fields
/// directly to select a different season.
///
/// Equality, ordering, hashing, and SQLx integer storage use the value returned by
/// [`encode`](Self::encode).
///
/// # Examples
///
/// ```
/// use nfl_model::{Season, SeasonType, SeasonWeek, Week};
///
/// let week = SeasonWeek {
///     season: Season(2026),
///     season_type: SeasonType::Reg,
///     week: Week(1),
/// };
/// assert_eq!(week.encode(), 2_026_001);
/// assert_eq!(SeasonWeek::decode(2_026_001), Some(week));
/// ```
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SeasonWeek {
    /// Year identifying the NFL season, including for postseason games in the next calendar year.
    pub season: Season,
    /// Whether this is a regular-season or postseason week.
    pub season_type: SeasonType,
    /// Week number within the selected season type.
    pub week: Week,
}

impl SeasonWeek {
    /// Create a validated week from a season, season type, and week number.
    ///
    /// Accepts regular-season weeks 1–18. For postseason weeks, accepts 1–3 as aliases of
    /// 19–21 (mapping 1 → 19, 2 → 20, 3 → 21), accepts 19–21 unchanged, and rejects every
    /// other value, including 0, 4–18, and 22. The Super Bowl is not a valid postseason week
    /// here. Returns `None` for any invalid combination.
    pub fn new(season: Season, season_type: SeasonType, week_num: u8) -> Option<Self> {
        let week = match (season_type, week_num) {
            (SeasonType::Reg, 1..=18) => Week(week_num),
            (SeasonType::Post, 1..=3) => Week(week_num + 18),
            (SeasonType::Post, 19..=21) => Week(week_num),
            _ => return None,
        };

        Some(Self {
            season,
            season_type,
            week,
        })
    }

    /// Enumerate the weeks of a season in schedule-independent order: regular-season weeks
    /// 1–18 followed by postseason weeks 19–21. Excludes the Super Bowl.
    ///
    /// Postseason weeks keep the supplied season year even when played in the next calendar
    /// year. Order reflects this canonical enumeration, not the NFL schedule.
    pub fn for_season(season: Season) -> impl Iterator<Item = Self> {
        (1..=18)
            .map(move |week_num| Self::new(season, SeasonType::Reg, week_num))
            .chain((19..=21).map(move |week_num| Self::new(season, SeasonType::Post, week_num)))
            .map(Option::unwrap)
    }

    /// Create a regular-season week using the default [`Season`].
    ///
    /// Accepts weeks 1–18; panics otherwise.
    pub fn reg(week_num: u8) -> Self {
        Self::new(Season::default(), SeasonType::Reg, week_num)
            .expect("invalid regular-season week")
    }

    /// Create a postseason week using the default [`Season`].
    ///
    /// Accepts weeks 1–3 (aliases of 19–21) and 19–21; `post(1)` and `post(19)` both produce
    /// canonical postseason week 19. Panics otherwise.
    pub fn post(week_num: u8) -> Self {
        Self::new(Season::default(), SeasonType::Post, week_num).expect("invalid postseason week")
    }

    /// Encode as `season * 1_000 + season_type * 100 + week`.
    ///
    /// The season-type digit is `0` for regular-season weeks and `1` for postseason weeks.
    pub fn encode(&self) -> u32 {
        u32::from(self.season.0) * 1_000
            + u32::from(self.season_type) * 100
            + u32::from(self.week.0)
    }

    /// Decode a value in the integer format produced by [`encode`](Self::encode).
    ///
    /// Returns `None` if the season-type digit is neither `0` nor `1` or the week is not a
    /// valid regular-season or postseason week. Stored postseason aliases 1–3 normalize to
    /// canonical weeks 19–21.
    pub fn decode(value: u32) -> Option<Self> {
        let year = (value / 1_000) as u16;
        let choice = ((value / 100) % 10) as u8;
        let week = (value % 100) as u8;

        let season_type = SeasonType::try_from(choice).ok()?;

        Self::new(Season(year), season_type, week)
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
