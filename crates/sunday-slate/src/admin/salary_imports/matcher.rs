use std::collections::HashMap;

use nfl_data::{Player, WeeklyRosterEntry};

use crate::player_identity::normalize_name as normalize;

/// A normalized-name index over nfl-data players, built once so matching a whole
/// slate is O(rows) rather than O(rows × players). Suffixes are kept in the name
/// (they live in the last name), and the team breaks same-name ties.
pub struct PlayerIndex<'a> {
    by_norm: HashMap<String, Vec<&'a Player>>,
}

impl<'a> PlayerIndex<'a> {
    pub fn build(players: &'a [Player]) -> Self {
        let mut by_norm: HashMap<String, Vec<&'a Player>> = HashMap::new();
        for p in players {
            by_norm.entry(normalize(&p.full_name)).or_default().push(p);
        }
        Self { by_norm }
    }

    /// Best-guess gsis_id for a FanDuel player: a normalized-name match, with the
    /// team breaking ties when several players share a name. `None` if unmatched
    /// or still ambiguous after the team filter.
    pub fn match_one(&self, fd_name: &str, fd_team: &str) -> Option<String> {
        let hits = self.by_norm.get(&normalize(fd_name))?;
        match hits.as_slice() {
            [] => None,
            [p] => Some(p.gsis_id.clone()),
            many => {
                let team_hits: Vec<&&Player> = many
                    .iter()
                    .filter(|p| p.latest_team.as_ref().map(|t| t.0.as_str()) == Some(fd_team))
                    .collect();
                match team_hits.as_slice() {
                    [p] => Some(p.gsis_id.clone()),
                    _ => None,
                }
            }
        }
    }
}

/// A normalized-name index over one slate's weekly rosters, scoped by team.
///
/// The secondary matcher, tried only after [`PlayerIndex`] misses. Two reasons
/// it catches what the player table cannot:
///
/// - `players` is nflverse's career index and omits players cut before
///   recording a snap — 144 of 2025's 3,133 rostered players. FanDuel lists
///   them because they were on the roster that week.
/// - The two feeds render 104 names differently ("Kenny Gainwell" in `players`,
///   "Kenneth Gainwell" on the roster), and FanDuel usually agrees with the
///   roster.
///
/// Scoped to the slate's own week, so a player who was on the team in some
/// other week never matches — the trade case the weekly dataset exists for.
pub struct RosterIndex {
    by_team_norm: HashMap<(String, String), Vec<RosterHit>>,
    by_team_stem: HashMap<(String, String), Vec<RosterHit>>,
}

/// Generational suffixes. FanDuel and the roster feed disagree about them in
/// both directions — "Kyle Pitts Sr." against "Kyle Pitts", and "Lew Nichols"
/// against "Lew Nichols III" — so neither side can be trusted to carry one.
const SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv", "v"];

/// A normalized name with any trailing generational suffixes removed.
fn strip_suffix(name: &str) -> String {
    let normalized = normalize(name);
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
            // No gsis id means nothing to resolve to.
            let Some(gsis_id) = e.gsis_id.clone() else {
                continue;
            };
            let hit = || RosterHit {
                gsis_id: gsis_id.clone(),
                position: e.position.clone(),
            };
            by_team_norm
                .entry((e.team.0.clone(), normalize(&e.full_name)))
                .or_default()
                .push(hit());
            by_team_stem
                .entry((e.team.0.clone(), strip_suffix(&e.full_name)))
                .or_default()
                .push(hit());
        }
        Self {
            by_team_norm,
            by_team_stem,
        }
    }

    /// Best-guess gsis_id for a FanDuel row: an exact normalized name, then the
    /// same name with generational suffixes stripped from both sides.
    ///
    /// `dfs_position` breaks ties between teammates of the same name; anything
    /// still ambiguous returns `None` and stays a human decision.
    pub fn match_one(&self, fd_name: &str, fd_team: &str, dfs_position: &str) -> Option<String> {
        let exact = self
            .by_team_norm
            .get(&(fd_team.to_string(), normalize(fd_name)));
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
    /// A likely-but-unproven match for a row [`Self::match_one`] could not
    /// settle: the one player on this team, this week, sharing the row's
    /// surname and FanDuel position.
    ///
    /// This is what catches nicknames — "Zonovan Knight" is rostered as "Bam
    /// Knight", "Juice Wells Jr." as "Antwane Wells Jr." — which no
    /// string-similarity threshold could reach from the first name.
    ///
    /// It can be wrong: if FanDuel's player is absent from the roster and a
    /// different player of the same surname and position is present, this names
    /// the wrong man. Callers must treat it as a suggestion for a human, never
    /// as a match.
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
            // More than one candidate is not guessable.
            Some(_) => None,
            None => Some(first.gsis_id.clone()),
        }
    }
}

/// Last word of a name, suffixes already removed.
fn surname(name: &str) -> String {
    surname_of_stem(&strip_suffix(name))
}

fn surname_of_stem(stem: &str) -> String {
    stem.rsplit(' ').next().unwrap_or_default().to_string()
}

/// One hit, or the one whose position agrees, or nothing.
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
    use nfl_data::TeamAbbr as NflTeamAbbr;

    fn player(gsis: &str, name: &str, team: &str) -> Player {
        Player {
            gsis_id: gsis.into(),
            espn_id: None,
            full_name: name.into(),
            first_name: None,
            last_name: None,
            position: None,
            latest_team: Some(NflTeamAbbr(team.into())),
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
            season: nfl_data::Season(2025),
            week: nfl_data::Week(1),
            team: NflTeamAbbr(team.into()),
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
        // Same name, wrong team: the slate is scoped, so no match.
        assert_eq!(idx.match_one("Kenneth Gainwell", "DAL", "RB"), None);
    }

    #[test]
    fn roster_matches_across_a_suffix_on_either_side() {
        // FanDuel carries the suffix, the roster does not.
        let es = vec![weekly(Some("00-1"), "Kyle Pitts", Some("TE"), "ATL")];
        let idx = RosterIndex::build(&es);
        assert_eq!(
            idx.match_one("Kyle Pitts Sr.", "ATL", "TE").as_deref(),
            Some("00-1")
        );

        // The roster carries the suffix, FanDuel does not.
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
        // Two teammates of that name AND that position: not resolvable.
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
        // The nickname case: FanDuel "Zonovan Knight" is rostered as "Bam Knight".
        // No name match, but exactly one RB named Knight on the team that week.
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
        // "Juice Wells Jr." (FanDuel) against "Antwane Wells Jr." (roster): the
        // surname agrees once the suffix is stripped from both sides.
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
        // A defender named Knight is not a suggestion for a running back.
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
