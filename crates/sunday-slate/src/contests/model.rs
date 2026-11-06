use nfl_data::slates::SlateId;
use nfl_data::{Season, SeasonType, SeasonWeek};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Contest {
    pub id: ContestId,
    pub name: String,
}

impl Contest {
    /// The main slate this contest scores against: a `Week N` name (N in
    /// 1..=18) in the default season, encoded as an nfl-data `SlateId`, kind
    /// digit `1` for a main slate. Names outside that set resolve no slate.
    pub fn slate_id(&self) -> Option<SlateId> {
        let week = self
            .name
            .strip_prefix("Week ")?
            .parse::<u8>()
            .ok()
            .filter(|week| (1..=18).contains(week))?;
        let week = SeasonWeek::new(Season::default(), SeasonType::Reg, week)?;
        Some(SlateId(i64::from(week.encode()) * 10 + 1))
    }

    /// The games on this contest's slate, from the published snapshot. Empty
    /// unless the slate is published with salaried players: an unpublished
    /// slate resolves no games for list, detail, or scoring purposes alike.
    pub fn games<'a>(&self, snapshot: &'a nfl_data::Snapshot) -> &'a [nfl_data::Game] {
        let Some(id) = self.slate_id() else {
            return &[];
        };
        if snapshot.slate(&id).is_none() {
            return &[];
        }
        snapshot.games_for_slate(&id)
    }
}

/// A contest's id as an opaque token. Contest names are display strings (and
/// the Week encoding) — identity is this id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct ContestId(pub i64);
