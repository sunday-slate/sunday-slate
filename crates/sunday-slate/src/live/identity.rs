use std::collections::{BTreeSet, HashMap};

use nfl_data::{
    EspnPlayerId, NflData, NflDataError, Player, Season, TeamAbbr, Week, WeeklyRosterEntry,
};

use crate::entries::NflPlayerId;
use crate::player_identity::{canonical_team, normalize_name, normalize_team};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityMatch {
    Matched(NflPlayerId),
    Unresolved,
    Ambiguous,
}

#[derive(Debug, Clone)]
pub struct IdentityRecord {
    pub gsis_id: NflPlayerId,
    pub name: String,
    pub team: Option<TeamAbbr>,
    pub position: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LiveIdentityIndex {
    by_espn: HashMap<String, BTreeSet<NflPlayerId>>,
    by_name: HashMap<(String, String, String), BTreeSet<NflPlayerId>>,
    by_gsis: HashMap<NflPlayerId, IdentityRecord>,
}

impl LiveIdentityIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_sources(players: &[Player], rosters: &[WeeklyRosterEntry]) -> Self {
        let mut facts = HashMap::<NflPlayerId, IdentityFacts>::new();
        for player in players {
            let gsis_id = NflPlayerId(player.gsis_id.clone());
            let facts = facts.entry(gsis_id).or_default();
            facts.add_name(&player.full_name);
            facts.add_espn(player.espn_id.as_deref());
            facts.add_global_team(player.latest_team.as_ref());
            facts.add_global_position(player.position.as_deref());
        }
        for roster in rosters {
            let Some(gsis) = roster.gsis_id.as_deref().filter(|id| !id.is_empty()) else {
                continue;
            };
            let facts = facts.entry(NflPlayerId(gsis.to_owned())).or_default();
            facts.add_name(&roster.full_name);
            facts.add_espn(roster.espn_id.as_deref());
            facts.weekly = true;
            facts.weekly_teams.insert(canonical_team(&roster.team).0);
            if let Some(position) = roster.position.as_deref() {
                facts.weekly_positions.insert(normalize_position(position));
            }
        }

        let mut index = Self::default();
        for (gsis_id, facts) in facts {
            let (team, position) = if facts.weekly {
                (
                    unique_team(&facts.weekly_teams),
                    unique_string(&facts.weekly_positions),
                )
            } else {
                (
                    unique_team(&facts.global_teams),
                    unique_string(&facts.global_positions),
                )
            };
            let record = IdentityRecord {
                gsis_id: gsis_id.clone(),
                name: facts.names.values().next().cloned().unwrap_or_default(),
                team: team.map(TeamAbbr),
                position,
            };
            index.by_gsis.insert(gsis_id.clone(), record);
            for espn_id in facts.espn_ids {
                index
                    .by_espn
                    .entry(espn_id)
                    .or_default()
                    .insert(gsis_id.clone());
            }
            if let (Some(team), Some(position)) = (
                index
                    .by_gsis
                    .get(&gsis_id)
                    .and_then(|record| record.team.as_ref()),
                index
                    .by_gsis
                    .get(&gsis_id)
                    .and_then(|record| record.position.as_deref()),
            ) {
                for name in facts.names.keys() {
                    index
                        .by_name
                        .entry((
                            name.clone(),
                            normalize_team(&team.0).to_owned(),
                            normalize_position(position),
                        ))
                        .or_default()
                        .insert(gsis_id.clone());
                }
            }
        }
        index
    }

    pub async fn build(
        nfl: &NflData,
        season: Season,
        week: Week,
        teams: &[TeamAbbr],
    ) -> Result<Self, NflDataError> {
        let players = nfl.players().await?;
        let mut rosters = Vec::new();
        let mut seen = BTreeSet::new();
        for team in teams {
            if !seen.insert(team.0.clone()) {
                continue;
            }
            rosters.extend(nfl.weekly_roster(season, week, team).await?);
        }
        Ok(Self::from_sources(&players, &rosters))
    }

    pub fn resolve(
        &self,
        espn_id: Option<&EspnPlayerId>,
        name: Option<&str>,
        team: Option<&TeamAbbr>,
        position: Option<&str>,
    ) -> IdentityMatch {
        if let Some(espn_id) = espn_id
            && let Some(candidates) = self.by_espn.get(&espn_id.0)
        {
            return match candidates.iter().next() {
                Some(gsis) if candidates.len() == 1 => IdentityMatch::Matched(gsis.clone()),
                Some(_) => IdentityMatch::Ambiguous,
                None => IdentityMatch::Unresolved,
            };
        }

        let (Some(name), Some(team), Some(position)) = (name, team, position) else {
            return IdentityMatch::Unresolved;
        };
        let key = (
            normalize_name(name),
            normalize_team(&team.0).to_owned(),
            normalize_position(position),
        );
        match self.by_name.get(&key) {
            Some(candidates) if candidates.len() == 1 => {
                IdentityMatch::Matched(candidates.iter().next().expect("one candidate").clone())
            }
            Some(candidates) if !candidates.is_empty() => IdentityMatch::Ambiguous,
            _ => IdentityMatch::Unresolved,
        }
    }

    pub fn record(&self, gsis_id: &NflPlayerId) -> Option<&IdentityRecord> {
        self.by_gsis.get(gsis_id)
    }

    pub fn contains_gsis(&self, gsis_id: &NflPlayerId) -> bool {
        self.by_gsis.contains_key(gsis_id)
    }

    pub fn player_ids(&self) -> impl Iterator<Item = &NflPlayerId> {
        self.by_gsis.keys()
    }

    pub fn scheduled_team(&self, gsis_id: &NflPlayerId) -> Option<&TeamAbbr> {
        self.record(gsis_id).and_then(|record| record.team.as_ref())
    }

    pub fn position(&self, gsis_id: &NflPlayerId) -> Option<&str> {
        self.record(gsis_id)
            .and_then(|record| record.position.as_deref())
    }
}

#[derive(Default)]
struct IdentityFacts {
    names: std::collections::BTreeMap<String, String>,
    espn_ids: BTreeSet<String>,
    global_teams: BTreeSet<String>,
    global_positions: BTreeSet<String>,
    weekly: bool,
    weekly_teams: BTreeSet<String>,
    weekly_positions: BTreeSet<String>,
}

impl IdentityFacts {
    fn add_name(&mut self, name: &str) {
        let normalized = normalize_name(name);
        if !normalized.is_empty() {
            self.names
                .entry(normalized)
                .or_insert_with(|| name.to_owned());
        }
    }

    fn add_espn(&mut self, espn_id: Option<&str>) {
        if let Some(espn_id) = espn_id.filter(|id| !id.is_empty()) {
            self.espn_ids.insert(espn_id.to_owned());
        }
    }

    fn add_global_team(&mut self, team: Option<&TeamAbbr>) {
        if let Some(team) = team {
            self.global_teams.insert(canonical_team(team).0);
        }
    }

    fn add_global_position(&mut self, position: Option<&str>) {
        if let Some(position) = position {
            self.global_positions.insert(normalize_position(position));
        }
    }
}

fn unique_team(values: &BTreeSet<String>) -> Option<String> {
    (values.len() == 1).then(|| values.iter().next().expect("one team").clone())
}

fn unique_string(values: &BTreeSet<String>) -> Option<String> {
    (values.len() == 1).then(|| values.iter().next().expect("one value").clone())
}

fn normalize_position(position: &str) -> String {
    position.trim().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfl_data::TeamAbbr;
    fn player(gsis: &str, espn: Option<&str>, name: &str, team: &str, position: &str) -> Player {
        Player {
            gsis_id: gsis.into(),
            espn_id: espn.map(str::to_owned),
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: Some(position.into()),
            latest_team: Some(TeamAbbr(team.into())),
            status: None,
            birth_date: None,
            headshot_url: None,
        }
    }

    fn roster(
        gsis: &str,
        espn: Option<&str>,
        name: &str,
        team: &str,
        position: &str,
    ) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            week: Week(1),
            team: TeamAbbr(team.into()),
            gsis_id: Some(gsis.into()),
            espn_id: espn.map(str::to_owned),
            full_name: name.into(),
            last_name: None,
            position: Some(position.into()),
            status: "ACT".into(),
        }
    }

    #[test]
    fn exact_provider_id_wins_and_conflicts_stay_ambiguous() {
        let index = LiveIdentityIndex::from_sources(
            &[player("a", Some("e1"), "A Player", "BUF", "RB")],
            &[],
        );
        assert_eq!(
            index.resolve(
                Some(&EspnPlayerId("e1".into())),
                Some("wrong"),
                Some(&TeamAbbr("BUF".into())),
                Some("RB"),
            ),
            IdentityMatch::Matched(NflPlayerId("a".into()))
        );

        let conflict = LiveIdentityIndex::from_sources(
            &[
                player("a", Some("e1"), "A Player", "BUF", "RB"),
                player("b", Some("e1"), "A Player", "BUF", "RB"),
            ],
            &[],
        );
        assert_eq!(
            conflict.resolve(
                Some(&EspnPlayerId("e1".into())),
                Some("A Player"),
                Some(&TeamAbbr("BUF".into())),
                Some("RB"),
            ),
            IdentityMatch::Ambiguous
        );
    }

    #[test]
    fn fallback_requires_name_team_and_position() {
        let index =
            LiveIdentityIndex::from_sources(&[], &[roster("a", None, "A Player", "LAR", "RB")]);
        assert_eq!(
            index.resolve(
                None,
                Some("A Player"),
                Some(&TeamAbbr("LA".into())),
                Some("RB"),
            ),
            IdentityMatch::Matched(NflPlayerId("a".into()))
        );
        assert_eq!(
            index.resolve(None, Some("A Player"), Some(&TeamAbbr("LA".into())), None,),
            IdentityMatch::Unresolved
        );
    }
    #[test]
    fn weekly_attributes_override_global_attributes_and_keep_both_names() {
        let index = LiveIdentityIndex::from_sources(
            &[player("a", None, "Global Name", "BUF", "RB")],
            &[roster("a", None, "Weekly Name", "KC", "WR")],
        );
        assert_eq!(
            index.scheduled_team(&NflPlayerId("a".into())),
            Some(&TeamAbbr("KC".into()))
        );
        assert_eq!(index.position(&NflPlayerId("a".into())), Some("WR"));
        assert_eq!(
            index.resolve(
                None,
                Some("Global Name"),
                Some(&TeamAbbr("KC".into())),
                Some("WR"),
            ),
            IdentityMatch::Matched(NflPlayerId("a".into()))
        );
        assert_eq!(
            index.resolve(
                None,
                Some("Weekly Name"),
                Some(&TeamAbbr("KC".into())),
                Some("WR"),
            ),
            IdentityMatch::Matched(NflPlayerId("a".into()))
        );
        assert_eq!(
            index.resolve(
                None,
                Some("Global Name"),
                Some(&TeamAbbr("BUF".into())),
                Some("RB"),
            ),
            IdentityMatch::Unresolved
        );
    }

    #[test]
    fn duplicate_weekly_records_do_not_create_ambiguity() {
        let rows = vec![
            roster("a", None, "A Player", "KC", "RB"),
            roster("a", None, "A Player", "KC", "RB"),
        ];
        let index = LiveIdentityIndex::from_sources(&[], &rows);
        assert_eq!(
            index.resolve(
                None,
                Some("A Player"),
                Some(&TeamAbbr("KC".into())),
                Some("RB"),
            ),
            IdentityMatch::Matched(NflPlayerId("a".into()))
        );
    }

    #[test]
    fn conflicting_weekly_attributes_are_order_independent_and_unusable() {
        let first = vec![
            roster("a", None, "A Player", "KC", "RB"),
            roster("a", None, "A Player", "BUF", "WR"),
        ];
        let second = first.iter().rev().cloned().collect::<Vec<_>>();
        for rows in [first, second] {
            let index = LiveIdentityIndex::from_sources(&[], &rows);
            assert_eq!(index.scheduled_team(&NflPlayerId("a".into())), None);
            assert_eq!(index.position(&NflPlayerId("a".into())), None);
            assert_eq!(
                index.resolve(
                    None,
                    Some("A Player"),
                    Some(&TeamAbbr("KC".into())),
                    Some("RB"),
                ),
                IdentityMatch::Unresolved
            );
        }

        let conflicting_positions = vec![
            roster("b", None, "B Player", "KC", "RB"),
            roster("b", None, "B Player", "KC", "WR"),
        ];
        let index = LiveIdentityIndex::from_sources(&[], &conflicting_positions);
        assert_eq!(
            index.scheduled_team(&NflPlayerId("b".into())),
            Some(&TeamAbbr("KC".into()))
        );
        assert_eq!(index.position(&NflPlayerId("b".into())), None);
    }

    #[test]
    fn conflicting_exact_ids_stay_ambiguous_in_either_input_order() {
        let players = vec![
            player("a", Some("e1"), "A Player", "BUF", "RB"),
            player("b", Some("e1"), "B Player", "KC", "RB"),
        ];
        for source in [players.clone(), players.into_iter().rev().collect()] {
            let index = LiveIdentityIndex::from_sources(&source, &[]);
            assert_eq!(
                index.resolve(Some(&EspnPlayerId("e1".into())), None, None, None),
                IdentityMatch::Ambiguous
            );
        }
    }
}
