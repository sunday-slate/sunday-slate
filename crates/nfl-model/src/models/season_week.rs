use std::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

use sqlx::{Database, Decode, Encode, Type, encode::IsNull, error::BoxDynError};

use crate::{Season, SeasonType, Week};

#[derive(Debug, Clone, Copy)]
pub struct SeasonWeek {
    pub season: Season,
    pub season_type: SeasonType,
    pub week: Week,
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

impl SeasonWeek {
    pub fn reg(week_num: u8) -> Self {
        Self {
            season: Season::default(),
            season_type: SeasonType::Reg,
            week: Week(week_num),
        }
    }

    pub fn post(week_num: u8) -> Self {
        Self {
            season: Season::default(),
            season_type: SeasonType::Post,
            week: Week(week_num),
        }
    }

    pub fn encode(&self) -> u32 {
        u32::from(self.season.0) * 1_000
            + u32::from(self.season_type) * 100
            + u32::from(self.week.0)
    }

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
    use std::collections::hash_map::DefaultHasher;

    use super::*;

    #[test]
    fn equality_hashing_and_ordering_use_the_encoded_value() {
        let regular = SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Reg,
            week: Week(1),
        };
        let postseason = SeasonWeek {
            season: Season(2025),
            season_type: SeasonType::Post,
            week: Week(1),
        };

        assert!(regular < postseason);
        assert_ne!(regular, postseason);

        let mut regular_hash = DefaultHasher::new();
        regular.hash(&mut regular_hash);
        let mut equal_hash = DefaultHasher::new();
        regular.clone().hash(&mut equal_hash);
        assert_eq!(regular_hash.finish(), equal_hash.finish());
    }
}
