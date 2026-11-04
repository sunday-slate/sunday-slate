use crate::NflData;
use crate::store::Store;
use fanduel_data::FanduelData;
use nfl_model::Season;
use nflverse_data::NflverseData;

/// The cloned data sources and configured season a snapshot build reads from,
/// so a build borrows no [`NflData`] handle.
#[derive(Clone)]
pub(crate) struct SnapshotSources {
    pub(crate) provider: NflverseData,
    pub(crate) fanduel: FanduelData,
    pub(crate) store: Store,
    pub(crate) season: Season,
}

impl From<&NflData> for SnapshotSources {
    fn from(nfl: &NflData) -> Self {
        Self {
            provider: nfl.provider.clone(),
            fanduel: nfl.fanduel.clone(),
            store: nfl.store.clone(),
            season: nfl.season,
        }
    }
}
