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
#[derive(Debug, Clone, Copy)]
pub struct SeasonWeek {
    /// Year identifying the NFL season, including for postseason games in the next calendar year.
    pub season: Season,
    /// Whether this is a regular-season or postseason week.
    pub season_type: SeasonType,
    /// Week number within the selected season type.
    pub week: Week,
}

impl SeasonWeek {
    /// Create a regular-season week using the default [`Season`].
    ///
    /// The week number is not validated against the NFL schedule.
    pub fn reg(week_num: u8) -> Self {
        Self {
            season: Season::default(),
            season_type: SeasonType::Reg,
            week: Week(week_num),
        }
    }

    /// Create a postseason week using the default [`Season`].
    ///
    /// The week number is not validated against the NFL schedule.
    pub fn post(week_num: u8) -> Self {
        Self {
            season: Season::default(),
            season_type: SeasonType::Post,
            week: Week(week_num),
        }
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
    /// Returns `None` if the season-type digit is neither `0` nor `1`. The season year and
    /// week number are not validated against the NFL schedule.
    pub fn decode(value: u32) -> Option<Self> {
        let year = (value / 1_000) as u16;
        let choice = ((value / 100) % 10) as u8;
        let week = (value % 100) as u8;

        let season_type = SeasonType::try_from(choice).ok()?;

        Some(Self {
            season: Season(year),
            season_type,
            week: Week(week),
        })
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
