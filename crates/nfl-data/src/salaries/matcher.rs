use std::collections::HashMap;

use crate::{Player, WeeklyRosterEntry};

/// Lowercase, drop non-alphanumerics, collapse whitespace. Must match
/// sunday-slate's live identity normalization exactly.
pub(crate) fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Name-index over nfl-data players; team breaks ties.
pub struct PlayerIndex<'a> {
    by_norm: HashMap<String, Vec<&'a Player>>,
}

impl<'a> PlayerIndex<'a> {
    pub fn build(players: &'a [Player]) -> Self {
        let mut by_norm: HashMap<String, Vec<&'a Player>> = HashMap::new();
        for p in players {
            by_norm
                .entry(normalize_name(&p.full_name))
                .or_default()
                .push(p);
        }
        Self { by_norm }
    }

    /// Name match with team tie-break; `None` if unmatched or ambiguous.
    pub fn match_one(&self, fd_name: &str, fd_team: &str) -> Option<String> {
        let hits = self.by_norm.get(&normalize_name(fd_name))?;
        match hits.as_slice() {
            [] => None,
            [p] => Some(p.gsis_id.clone()),
            many => {
                let team_hits: Vec<&&Player> = many
                    .iter()
                    .filter(|p| p.latest_team.as_ref().map(|t| t.as_str()) == Some(fd_team))
                    .collect();
                match team_hits.as_slice() {
                    [p] => Some(p.gsis_id.clone()),
                    _ => None,
                }
            }
        }
    }
}

/// Week-scoped name-index over one slate's rosters, tried after `PlayerIndex`:
/// the player table omits players cut before a snap, and FanDuel's spellings
/// usually agree with the roster, not the player table.
pub struct RosterIndex {
    by_team_norm: HashMap<(String, String), Vec<RosterHit>>,
    by_team_stem: HashMap<(String, String), Vec<RosterHit>>,
}

/// Generational suffixes; the feeds disagree about them in both directions.
const SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv", "v"];

/// Normalized name minus trailing generational suffixes.
fn strip_suffix(name: &str) -> String {
    let normalized = normalize_name(name);
    let mut parts: Vec<&str> = normalized.split(' ').collect();
    while parts.len() > 1 && SUFFIXES.contains(parts.last().unwrap()) {
        parts.pop();
    }
    parts.join(" ")
}

struct RosterHit {
    gsis_id: String,
    position: Option<String>,
}

impl RosterIndex {
    pub fn build(entries: &[WeeklyRosterEntry]) -> Self {
        let mut by_team_norm: HashMap<(String, String), Vec<RosterHit>> = HashMap::new();
        let mut by_team_stem: HashMap<(String, String), Vec<RosterHit>> = HashMap::new();
        for e in entries {
            let Some(gsis_id) = e.gsis_id.clone() else {
                continue;
            };
            let hit = || RosterHit {
                gsis_id: gsis_id.clone(),
                position: e.position.clone(),
            };
            by_team_norm
                .entry((e.team.as_str().to_owned(), normalize_name(&e.full_name)))
                .or_default()
                .push(hit());
            by_team_stem
                .entry((e.team.as_str().to_owned(), strip_suffix(&e.full_name)))
                .or_default()
                .push(hit());
        }
        Self {
            by_team_norm,
            by_team_stem,
        }
    }

    /// Exact normalized name, then suffix-stripped; `dfs_position` breaks
    /// teammate ties; ambiguous stays `None`.
    pub fn match_one(&self, fd_name: &str, fd_team: &str, dfs_position: &str) -> Option<String> {
        let exact = self
            .by_team_norm
            .get(&(fd_team.to_string(), normalize_name(fd_name)));
        let stem = || {
            self.by_team_stem
                .get(&(fd_team.to_string(), strip_suffix(fd_name)))
        };
        exact
            .and_then(|h| pick(h, dfs_position))
            .or_else(|| stem().and_then(|h| pick(h, dfs_position)))
    }
}

impl RosterIndex {
    /// Surname+position suggestion when match_one fails — catches nicknames
    /// like "Zonovan Knight" → "Bam Knight". Unproven: if the row's player is
    /// absent and another surname/position match exists, this names the wrong
    /// man. Suggestions only, never auto-match.
    pub fn suggest_one(&self, fd_name: &str, fd_team: &str, dfs_position: &str) -> Option<String> {
        let surname = surname(fd_name);
        if surname.is_empty() {
            return None;
        }
        let mut hits = self
            .by_team_stem
            .iter()
            .filter(|((team, stem), _)| team == fd_team && surname_of_stem(stem) == surname)
            .flat_map(|(_, hits)| hits.iter())
            .filter(|h| h.position.as_deref() == Some(dfs_position));
        let first = hits.next()?;
        match hits.next() {
            Some(_) => None,
            None => Some(first.gsis_id.clone()),
        }
    }
}

/// Last word of the suffix-stripped name.
fn surname(name: &str) -> String {
    surname_of_stem(&strip_suffix(name))
}

fn surname_of_stem(stem: &str) -> String {
    stem.rsplit(' ').next().unwrap_or_default().to_string()
}

/// The one hit, or the one whose position agrees, or `None`.
fn pick(hits: &[RosterHit], dfs_position: &str) -> Option<String> {
    match hits {
        [] => None,
        [h] => Some(h.gsis_id.clone()),
        many => {
            let pos_hits: Vec<&RosterHit> = many
                .iter()
                .filter(|h| h.position.as_deref() == Some(dfs_position))
                .collect();
            match pos_hits.as_slice() {
                [h] => Some(h.gsis_id.clone()),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(gsis: &str, name: &str, team: &str) -> Player {
        Player {
            gsis_id: gsis.into(),
            espn_id: None,
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: None,
            latest_team: Some(team.parse().unwrap()),
            headshot_url: None,
        }
    }

    #[test]
    fn matches_on_normalized_name_keeping_suffix() {
        let ps = vec![player("00-1", "Kyle Pitts Sr.", "ATL")];
        let idx = PlayerIndex::build(&ps);
        assert_eq!(
            idx.match_one("Kyle Pitts Sr.", "ATL").as_deref(),
            Some("00-1")
        );
    }

    #[test]
    fn team_breaks_a_duplicate_name_tie() {
        let ps = vec![
            player("00-1", "Mike Williams", "NYJ"),
            player("00-2", "Mike Williams", "PIT"),
        ];
        let idx = PlayerIndex::build(&ps);
        assert_eq!(
            idx.match_one("Mike Williams", "PIT").as_deref(),
            Some("00-2")
        );
        assert_eq!(idx.match_one("Mike Williams", "SF"), None);
    }

    #[test]
    fn no_match_returns_none() {
        let ps = vec![player("00-1", "Kyle Pitts", "ATL")];
        let idx = PlayerIndex::build(&ps);
        assert_eq!(idx.match_one("Nobody Here", "ATL"), None);
    }

    fn weekly(gsis: Option<&str>, name: &str, pos: Option<&str>, team: &str) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: crate::Season(2025),
            week: crate::Week(1),
            team: team.parse().unwrap(),
            gsis_id: gsis.map(Into::into),
            espn_id: None,
            full_name: name.into(),
            last_name: None,
            position: pos.map(Into::into),
        }
    }

    #[test]
    fn roster_matches_exact_name_scoped_to_team() {
        let es = vec![weekly(Some("00-1"), "Kenneth Gainwell", Some("RB"), "PHI")];
        let idx = RosterIndex::build(&es);
        assert_eq!(
            idx.match_one("Kenneth Gainwell", "PHI", "RB").as_deref(),
            Some("00-1")
        );
        assert_eq!(idx.match_one("Kenneth Gainwell", "DAL", "RB"), None);
    }

    #[test]
    fn roster_matches_across_a_suffix_on_either_side() {
        let es = vec![weekly(Some("00-1"), "Kyle Pitts", Some("TE"), "ATL")];
        let idx = RosterIndex::build(&es);
        assert_eq!(
            idx.match_one("Kyle Pitts Sr.", "ATL", "TE").as_deref(),
            Some("00-1")
        );

        let es = vec![weekly(Some("00-2"), "Lew Nichols III", Some("RB"), "DET")];
        let idx = RosterIndex::build(&es);
        assert_eq!(
            idx.match_one("Lew Nichols", "DET", "RB").as_deref(),
            Some("00-2")
        );
    }

    #[test]
    fn roster_position_breaks_a_same_name_tie() {
        let es = vec![
            weekly(Some("00-rb"), "Chris Johnson", Some("RB"), "NYJ"),
            weekly(Some("00-cb"), "Chris Johnson", Some("CB"), "NYJ"),
        ];
        let idx = RosterIndex::build(&es);
        assert_eq!(
            idx.match_one("Chris Johnson", "NYJ", "RB").as_deref(),
            Some("00-rb")
        );
        let es = vec![
            weekly(Some("00-a"), "Chris Johnson", Some("RB"), "NYJ"),
            weekly(Some("00-b"), "Chris Johnson", Some("RB"), "NYJ"),
        ];
        let idx = RosterIndex::build(&es);
        assert_eq!(idx.match_one("Chris Johnson", "NYJ", "RB"), None);
    }

    #[test]
    fn roster_skips_entries_without_a_gsis_id() {
        let es = vec![weekly(None, "Ghost Player", Some("WR"), "GB")];
        let idx = RosterIndex::build(&es);
        assert_eq!(idx.match_one("Ghost Player", "GB", "WR"), None);
    }

    #[test]
    fn suggest_finds_the_lone_surname_and_position_match() {
        let es = vec![weekly(Some("00-bam"), "Bam Knight", Some("RB"), "NYJ")];
        let idx = RosterIndex::build(&es);
        assert_eq!(idx.match_one("Zonovan Knight", "NYJ", "RB"), None);
        assert_eq!(
            idx.suggest_one("Zonovan Knight", "NYJ", "RB").as_deref(),
            Some("00-bam")
        );
    }

    #[test]
    fn suggest_matches_surname_after_stripping_suffixes() {
        let es = vec![weekly(
            Some("00-wells"),
            "Antwane Wells Jr.",
            Some("WR"),
            "SF",
        )];
        let idx = RosterIndex::build(&es);
        assert_eq!(
            idx.suggest_one("Juice Wells Jr.", "SF", "WR").as_deref(),
            Some("00-wells")
        );
    }

    #[test]
    fn suggest_declines_when_two_share_the_surname_and_position() {
        let es = vec![
            weekly(Some("00-a"), "Bam Knight", Some("RB"), "NYJ"),
            weekly(Some("00-b"), "Trey Knight", Some("RB"), "NYJ"),
        ];
        let idx = RosterIndex::build(&es);
        assert_eq!(idx.suggest_one("Zonovan Knight", "NYJ", "RB"), None);
    }

    #[test]
    fn suggest_requires_the_position_to_agree() {
        let es = vec![weekly(Some("00-cb"), "Bam Knight", Some("CB"), "NYJ")];
        let idx = RosterIndex::build(&es);
        assert_eq!(idx.suggest_one("Zonovan Knight", "NYJ", "RB"), None);
    }

    #[test]
    fn suggest_is_scoped_to_the_team() {
        let es = vec![weekly(Some("00-bam"), "Bam Knight", Some("RB"), "NYJ")];
        let idx = RosterIndex::build(&es);
        assert_eq!(idx.suggest_one("Zonovan Knight", "DAL", "RB"), None);
    }
}
