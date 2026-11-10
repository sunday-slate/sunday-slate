use nfl_model::{NflPlayerId, TeamAbbr};
use nflverse_data::NflverseData;
use std::collections::HashMap;

use crate::{NflData, NflDataError};

/// How a page names one pick: the display facts every consumer of a gsis id
/// needs, resolved from whichever source carries the player.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerIdentity {
    pub name: String,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
    /// Player photo URL from the `players` release. `None` for picks named
    /// only from weekly rosters (that source carries no headshot).
    pub headshot_url: Option<String>,
}

/// Display facts per gsis id from nflverse: `players` first, weekly rosters
/// for the misses; unknown ids are absent from the map.
pub(crate) async fn identify(
    nflverse: &NflverseData,
    ids: &[NflPlayerId],
) -> Result<HashMap<NflPlayerId, PlayerIdentity>, NflDataError> {
    let players = nflverse.players_by_gsis(ids).await?;
    let unnamed: Vec<NflPlayerId> = ids
        .iter()
        .filter(|id| !players.contains_key(id.0.as_str()))
        .cloned()
        .collect();
    let roster = nflverse.weekly_roster_entries_by_gsis(&unnamed).await?;
    let named = players.into_iter().map(|(id, p)| {
        let identity = PlayerIdentity {
            name: p.full_name,
            team: p.latest_team,
            position: p.position,
            headshot_url: p.headshot_url,
        };
        (NflPlayerId(id), identity)
    });
    let rostered = roster.into_iter().map(|(id, e)| {
        let identity = PlayerIdentity {
            name: e.full_name,
            team: Some(e.team),
            position: e.position,
            headshot_url: None,
        };
        (NflPlayerId(id), identity)
    });
    Ok(named.chain(rostered).collect())
}

impl NflData {
    /// Display facts per gsis id: `players` first, weekly rosters for the
    /// misses; unknown ids are absent from the map.
    pub async fn identify(
        &self,
        ids: &[NflPlayerId],
    ) -> Result<HashMap<NflPlayerId, PlayerIdentity>, NflDataError> {
        crate::identity::identify(&self.nflverse, ids).await
    }
}

#[cfg(test)]
mod tests {
    use crate::{NflData, Season, Week};
    use crate::{Player, TeamAbbr, WeeklyRosterEntry};

    fn player(gsis: &str, name: &str) -> Player {
        Player {
            espn_id: None,
            gsis_id: gsis.into(),
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: Some("RB".into()),
            latest_team: Some(TeamAbbr::KC),
            headshot_url: None,
        }
    }

    fn roster_entry(gsis: &str, name: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            espn_id: None,
            week: Week(1),
            team: TeamAbbr::BUF,
            gsis_id: Some(gsis.into()),
            full_name: name.into(),
            last_name: None,
            position: Some("WR".into()),
        }
    }

    #[tokio::test]
    async fn names_from_players_first_then_the_weekly_roster() {
        let nfl = NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[player("00-P", "Player Table")], &[])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster_entry("00-R", "Roster Only")])
            .await
            .unwrap();

        let got = nfl
            .identify(&[
                nfl_model::NflPlayerId("00-P".into()),
                nfl_model::NflPlayerId("00-R".into()),
                nfl_model::NflPlayerId("00-X".into()),
            ])
            .await
            .unwrap();

        let from_players = &got[&nfl_model::NflPlayerId("00-P".into())];
        assert_eq!(from_players.name, "Player Table");
        assert_eq!(from_players.team, Some(TeamAbbr::KC));
        assert_eq!(from_players.position.as_deref(), Some("RB"));

        let from_roster = &got[&nfl_model::NflPlayerId("00-R".into())];
        assert_eq!(from_roster.name, "Roster Only");
        assert_eq!(from_roster.team, Some(TeamAbbr::BUF));
        assert_eq!(from_roster.position.as_deref(), Some("WR"));

        // An id in neither source has no identity to report.
        assert!(!got.contains_key(&nfl_model::NflPlayerId("00-X".into())));
    }

    #[tokio::test]
    async fn identify_carries_headshot_url_from_players_table() {
        let nfl = NflData::in_memory().await.unwrap();
        let player = Player {
            gsis_id: "00-HEAD".into(),
            espn_id: None,
            full_name: "Photo Guy".into(),
            first_name: None,
            last_name: None,
            position: Some("QB".into()),
            latest_team: Some(TeamAbbr::KC),
            headshot_url: Some("https://img.example/photo.png".into()),
        };
        nfl.seed_for_test(&[player], &[]).await.unwrap();

        let out = nfl
            .identify(&[nfl_model::NflPlayerId("00-HEAD".into())])
            .await
            .unwrap();
        let id = out
            .get(&nfl_model::NflPlayerId("00-HEAD".into()))
            .expect("identified");
        assert_eq!(
            id.headshot_url.as_deref(),
            Some("https://img.example/photo.png")
        );
    }
}
