mod candidates;
pub mod csv;
mod handlers;
pub mod matcher;
pub mod model;
pub mod schedule;
pub mod store;
mod view;

pub use handlers::router;

use std::collections::{BTreeSet, HashMap};

use nfl_data::{NflData, Season, TeamAbbr as NflTeamAbbr};

use crate::Db;
use crate::admin::salary_imports::csv::SalaryRow;
use crate::admin::salary_imports::matcher::{PlayerIndex, RosterIndex};
use crate::admin::salary_imports::model::RowState;
use crate::admin::salary_imports::schedule::resolve_slate_games;
use crate::nfl_players::store as player_store;
use crate::player_identity::normalize_team;
use crate::player_salaries::model::{DfsPosition, NflPlayerSalary};
use crate::player_salaries::store as salary_store;

/// Whole-slate failures that stop classification. Individual malformed rows are
/// NOT errors — they land in `SalaryPlan::offending`.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("empty salary CSV")]
    Empty,
    #[error("could not parse salary CSV: {0}")]
    Csv(String),
    #[error("no single NFL week contains this slate's games")]
    UnresolvableSlate,
    /// A nfl-data or crosswalk read failed while classifying — not a CSV problem.
    #[error("could not read salary data: {0}")]
    Data(String),
}

/// One classified salary row, ready to stage or write. `gsis_player_id` is set
/// for `Matched`; `None` for `Dst` and `Unmatched`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRow {
    pub fd_player_id: String,
    pub fd_name: String,
    pub fd_team: String,
    pub dfs_position: DfsPosition,
    pub salary: i64,
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub state: RowState,
    /// A likely-but-unproven match offered for one-click confirmation. Only
    /// ever set on `Unmatched` rows, and never imported without a human — see
    /// `RosterIndex::suggest_one`.
    pub suggested_gsis_player_id: Option<String>,
}

/// The result of classifying a CSV against the slate and nfl-data.
#[derive(Debug, Default, PartialEq)]
pub struct SalaryPlan {
    pub season: u16,
    /// The one NFL week this slate resolved to.
    pub week: Option<u8>,
    pub games: usize,
    pub rows: Vec<PlannedRow>,
    /// Human-readable descriptions of rows that could not be classified at all
    /// (bad position, or a game string without a resolvable team).
    pub offending: Vec<String>,
}

/// Backward-compatible report for the CLI / validation harness.
#[derive(Debug, Default, PartialEq)]
pub struct ImportReport {
    pub games: usize,
    pub salaries: usize,
    pub unmatched: Vec<String>,
}

/// Classify every CSV row against the slate and nfl-data, writing nothing.
/// Player rows match crosswalk-first (`fd_player_id`), then by normalized name.
pub async fn plan(
    db: &Db,
    nfl: &NflData,
    csv: impl std::io::Read,
    season: u16,
) -> Result<SalaryPlan, PlanError> {
    let rows: Vec<SalaryRow> = csv::parse(csv).map_err(|e| PlanError::Csv(e.to_string()))?;
    if rows.is_empty() {
        return Err(PlanError::Empty);
    }

    let matchups: BTreeSet<(String, String)> = rows
        .iter()
        .filter_map(|r| r.game.split_once('@'))
        .map(|(away, home)| {
            (
                normalize_team(away).to_string(),
                normalize_team(home).to_string(),
            )
        })
        .collect();

    let games = nfl
        .games(Season(season))
        .await
        .map_err(|e| PlanError::Data(e.to_string()))?;
    let slate_games =
        resolve_slate_games(&games, &matchups).map_err(|_| PlanError::UnresolvableSlate)?;

    let mut game_of: HashMap<String, String> = HashMap::new();
    for g in &slate_games {
        game_of.insert(g.home_team.0.clone(), g.gsis_game_id.clone());
        game_of.insert(g.away_team.0.clone(), g.gsis_game_id.clone());
    }

    let players = nfl
        .players()
        .await
        .map_err(|e| PlanError::Data(e.to_string()))?;
    let index = PlayerIndex::build(&players);

    // Secondary matcher: the slate's own week of rosters, one query per team.
    // `resolve_slate_games` pins the slate to a single week, so every game here
    // shares one.
    let mut roster_entries = Vec::new();
    if let Some(week) = slate_games.first().map(|g| g.week) {
        let mut teams: BTreeSet<&String> = BTreeSet::new();
        for g in &slate_games {
            teams.insert(&g.home_team.0);
            teams.insert(&g.away_team.0);
        }
        for team in teams {
            let entries = nfl
                .weekly_roster(Season(season), week, &NflTeamAbbr(team.clone()))
                .await
                .map_err(|e| PlanError::Data(e.to_string()))?;
            roster_entries.extend(entries);
        }
    }
    let roster_index = RosterIndex::build(&roster_entries);

    let mut plan = SalaryPlan {
        season,
        week: slate_games.first().map(|g| g.week.0),
        games: slate_games.len(),
        ..Default::default()
    };

    for r in &rows {
        let Some(pos) = r.dfs_position() else {
            plan.offending
                .push(format!("{} (bad position {})", r.name(), r.position));
            continue;
        };
        let team = normalize_team(&r.team).to_string();
        let Some(gsis_game_id) = game_of.get(&team).cloned() else {
            plan.offending
                .push(format!("{} (no game for {})", r.name(), team));
            continue;
        };

        if pos.is_defense() {
            plan.rows.push(PlannedRow {
                fd_player_id: r.fd_player_id().to_string(),
                fd_name: r.name(),
                fd_team: team,
                dfs_position: DfsPosition::Dst,
                salary: r.salary,
                gsis_game_id,
                gsis_player_id: None,
                state: RowState::Dst,
                suggested_gsis_player_id: None,
            });
            continue;
        }

        // Crosswalk first (sticky prior resolutions), then the player table,
        // then this week's roster for the row's team — see `RosterIndex` for
        // what the player table structurally cannot answer.
        let gsis = match player_store::by_fd_id(db.reader(), r.fd_player_id()).await {
            Ok(Some(np)) => Some(np.gsis_player_id),
            Ok(None) => index
                .match_one(&r.name(), &team)
                .or_else(|| roster_index.match_one(&r.name(), &team, pos.label())),
            Err(e) => return Err(PlanError::Data(e.to_string())),
        };

        let (state, gsis_player_id) = match gsis {
            Some(g) => (RowState::Matched, Some(g)),
            None => (RowState::Unmatched, None),
        };
        // Only an unmatched row needs a suggestion, and a suggestion never
        // becomes a match without a human.
        let suggested_gsis_player_id = match state {
            RowState::Unmatched => roster_index.suggest_one(&r.name(), &team, pos.label()),
            _ => None,
        };
        plan.rows.push(PlannedRow {
            fd_player_id: r.fd_player_id().to_string(),
            fd_name: r.name(),
            fd_team: team,
            dfs_position: pos,
            salary: r.salary,
            gsis_game_id,
            gsis_player_id,
            state,
            suggested_gsis_player_id,
        });
    }

    Ok(plan)
}

/// Write one planned row (salary + crosswalk when it carries a gsis).
async fn write_planned(
    conn: &mut sqlx::SqliteConnection,
    row: &PlannedRow,
) -> Result<(), sqlx::Error> {
    if let Some(gsis) = &row.gsis_player_id {
        player_store::upsert(&mut *conn, gsis, &row.fd_player_id).await?;
    }
    salary_store::upsert(
        &mut *conn,
        &NflPlayerSalary {
            id: 0,
            gsis_game_id: row.gsis_game_id.clone(),
            gsis_player_id: row.gsis_player_id.clone(),
            team_abbr: row.fd_team.clone(),
            dfs_position: row.dfs_position,
            salary: row.salary,
        },
    )
    .await
}

/// CLI / validation entry point: classify, write matched + DST rows, drop
/// unmatched (reported). Behavior matches the pre-refactor `import()`.
pub async fn import(
    db: &Db,
    nfl: &NflData,
    csv: impl std::io::Read,
    season: u16,
) -> anyhow::Result<ImportReport> {
    let plan = plan(db, nfl, csv, season).await?;

    let mut report = ImportReport {
        games: plan.games,
        unmatched: plan.offending,
        ..Default::default()
    };
    // Write matched/DST (and resolved/skipped, unreachable via this path but
    // harmless); report unmatched player rows.
    let mut writable: Vec<PlannedRow> = Vec::with_capacity(plan.rows.len());
    for row in plan.rows {
        match row.state {
            RowState::Unmatched => report.unmatched.push(row.fd_name),
            _ => writable.push(row),
        }
    }

    let written = db
        .write_tx::<_, usize, anyhow::Error>(async |conn| {
            let mut n = 0;
            for row in &writable {
                write_planned(&mut *conn, row).await?;
                n += 1;
            }
            Ok(n)
        })
        .await?;
    report.salaries = written;
    Ok(report)
}

use crate::admin::salary_imports::model::SalaryImportRow;
use crate::admin::salary_imports::store as import_store;

/// Convert a staged row that will be written into a `PlannedRow`.
fn planned_from_staged(r: &SalaryImportRow) -> PlannedRow {
    PlannedRow {
        fd_player_id: r.fd_player_id.clone(),
        fd_name: r.fd_name.clone(),
        fd_team: r.fd_team.clone(),
        dfs_position: r.dfs_position,
        salary: r.salary,
        gsis_game_id: r.gsis_game_id.clone(),
        gsis_player_id: r.gsis_player_id.clone(),
        state: r.state,
        // Commit reads only matched/dst/resolved rows, none of which carry a
        // suggestion, and nothing downstream of staging reads one.
        suggested_gsis_player_id: None,
    }
}

/// Promote a pending batch: write every `matched`/`dst`/`resolved` row (salary
/// and crosswalk), mark the batch committed. Safe by construction —
/// `unmatched` and `skipped` rows are never promoted.
pub async fn commit_staged(db: &Db, import_id: i64) -> anyhow::Result<ImportReport> {
    let rows = import_store::rows_for(db.reader(), import_id).await?;
    let writable: Vec<PlannedRow> = rows
        .iter()
        .filter(|r| {
            matches!(
                r.state,
                RowState::Matched | RowState::Dst | RowState::Resolved
            )
        })
        .map(planned_from_staged)
        .collect();

    let written = db
        .write_tx::<_, usize, anyhow::Error>(async |conn| {
            let mut n = 0;
            for row in &writable {
                write_planned(&mut *conn, row).await?;
                n += 1;
            }
            sqlx::query!(
                r#"UPDATE salary_imports
                   SET status = 'committed', committed_at = datetime('now','subsec')
                   WHERE id = ?"#,
                import_id,
            )
            .execute(&mut *conn)
            .await?;
            Ok(n)
        })
        .await?;

    Ok(ImportReport {
        games: 0,
        salaries: written,
        unmatched: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::salary_imports::model::RowState;
    use nfl_data::{
        Game, Player, Season, SeasonType, TeamAbbr as NflTeamAbbr, Week, WeeklyRosterEntry,
    };

    fn game(id: &str, week: u8, away: &str, home: &str) -> Game {
        Game {
            gsis_game_id: id.into(),
            season: Season(2025),
            week: Week(week),
            season_type: SeasonType::Reg,
            kickoff: None,
            home_team: NflTeamAbbr(home.into()),
            away_team: NflTeamAbbr(away.into()),
            home_score: None,
            away_score: None,
        }
    }

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

    const HEADER: &str = "\"Id\",\"Position\",\"First Name\",\"Nickname\",\"Last Name\",\"FPPG\",\"Played\",\"Salary\",\"Game\",\"Team\",\"Opponent\",\"Injury Indicator\",\"Injury Details\"";

    #[test]
    fn normalize_team_maps_fanduel_codes() {
        assert_eq!(normalize_team("JAC"), "JAX");
        assert_eq!(normalize_team("LAR"), "LA");
        assert_eq!(normalize_team("BUF"), "BUF");
    }

    #[tokio::test]
    async fn plan_partitions_matched_dst_and_unmatched() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(
            &[player("00-1", "Josh Allen", "BUF")],
            &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")],
        )
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-100\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\
             \"1-200\",\"WR\",\"Nobody\",\"\",\"Here\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n\
             \"1-300\",\"D\",\"New York\",\"\",\"Jets\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );

        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        assert_eq!(plan.games, 1);
        assert!(plan.offending.is_empty());
        let by_id = |id: &str| {
            plan.rows
                .iter()
                .find(|r| r.fd_player_id == id)
                .unwrap()
                .clone()
        };
        assert_eq!(by_id("100").state, RowState::Matched);
        assert_eq!(by_id("100").gsis_player_id.as_deref(), Some("00-1"));
        assert_eq!(by_id("200").state, RowState::Unmatched);
        assert_eq!(by_id("300").state, RowState::Dst);
    }

    fn roster(gsis: &str, name: &str, pos: &str, team: &str, week: u8) -> WeeklyRosterEntry {
        WeeklyRosterEntry {
            season: Season(2025),
            week: Week(week),
            team: NflTeamAbbr(team.into()),
            gsis_id: Some(gsis.into()),
            espn_id: None,
            full_name: name.into(),
            last_name: name.split_whitespace().next_back().map(String::from),
            position: Some(pos.into()),
        }
    }

    /// nflverse's `players` release is a career index; it omits players cut
    /// before recording a snap. 144 of 2025's 3,133 rostered players are absent
    /// from it, and FanDuel lists them because they were on the roster.
    #[tokio::test]
    async fn plan_falls_back_to_the_weekly_roster_for_a_player_absent_from_players() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster("00-4040", "Dymere Miller", "WR", "NYJ", 1)])
            .await
            .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"WR\",\"Dymere\",\"\",\"Miller\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let row = &plan.rows[0];
        assert_eq!(row.state, RowState::Matched);
        assert_eq!(row.gsis_player_id.as_deref(), Some("00-4040"));
    }

    /// The two feeds render names differently for 104 players. FanDuel usually
    /// agrees with the roster, so "Kenneth Gainwell" misses `players`
    /// ("Kenny Gainwell") and needs the roster to match.
    #[tokio::test]
    async fn plan_falls_back_to_the_weekly_roster_when_players_spells_the_name_differently() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(
            &[player("00-0036919", "Kenny Gainwell", "PIT")],
            &[game("2025_01_PIT_NYJ", 1, "PIT", "NYJ")],
        )
        .await
        .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster(
            "00-0036919",
            "Kenneth Gainwell",
            "RB",
            "PIT",
            1,
        )])
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"RB\",\"Kenneth\",\"\",\"Gainwell\",\"0\",\"0\",\"6000\",\"PIT@NYJ\",\"PIT\",\"NYJ\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let row = &plan.rows[0];
        assert_eq!(row.state, RowState::Matched);
        assert_eq!(row.gsis_player_id.as_deref(), Some("00-0036919"));
    }

    /// Generational suffixes are a rendering artifact, not part of the person:
    /// FanDuel and the roster feed disagree about them in both directions. 21 of
    /// import 1's 44 remaining unmatched rows differed only this way.
    #[tokio::test]
    async fn plan_matches_across_a_generational_suffix_in_either_direction() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[
            // FanDuel carries the suffix, the roster does not.
            roster("00-1", "Kyle Pitts", "TE", "NYJ", 1),
            // The roster carries it, FanDuel does not.
            roster("00-2", "Lew Nichols III", "RB", "NYJ", 1),
        ])
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-100\",\"TE\",\"Kyle\",\"\",\"Pitts Sr.\",\"0\",\"0\",\"6000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n\
             \"1-200\",\"RB\",\"Lew\",\"\",\"Nichols\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let by_id = |id: &str| plan.rows.iter().find(|r| r.fd_player_id == id).unwrap();
        assert_eq!(by_id("100").gsis_player_id.as_deref(), Some("00-1"));
        assert_eq!(by_id("200").gsis_player_id.as_deref(), Some("00-2"));
    }

    /// Stripping the suffix must not merge two different people. Without a
    /// unique answer the row stays a human decision.
    #[tokio::test]
    async fn plan_will_not_guess_between_two_players_who_differ_only_by_suffix() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[
            roster("00-1", "Same Guy", "WR", "NYJ", 1),
            roster("00-2", "Same Guy Jr.", "WR", "NYJ", 1),
        ])
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-100\",\"WR\",\"Same\",\"\",\"Guy II\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        assert_eq!(plan.rows[0].state, RowState::Unmatched);
    }

    /// Nicknames defeat every string-similarity rule — "Zonovan Knight" is
    /// rostered as "Bam Knight". Surname + FanDuel position, unique on that
    /// team that week, finds them. It can be wrong, so it only suggests.
    #[tokio::test]
    async fn plan_suggests_a_unique_surname_and_position_match_without_matching_it() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[
            roster("00-bam", "Bam Knight", "RB", "NYJ", 1),
            // Same surname, different position: must not muddy the RB answer.
            roster("00-other", "Herb Knight", "WR", "NYJ", 1),
        ])
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"RB\",\"Zonovan\",\"\",\"Knight\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let row = &plan.rows[0];
        assert_eq!(
            row.state,
            RowState::Unmatched,
            "a suggestion is not a match"
        );
        assert_eq!(row.gsis_player_id, None, "nothing is imported on its own");
        assert_eq!(
            row.suggested_gsis_player_id.as_deref(),
            Some("00-bam"),
            "but the admin is offered the obvious answer"
        );
    }

    /// Two players of one surname and position on a roster is not guessable,
    /// and an outright match never needs a suggestion.
    #[tokio::test]
    async fn plan_suggests_nothing_when_ambiguous_or_already_matched() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[
            roster("00-1", "Alpha Twin", "WR", "NYJ", 1),
            roster("00-2", "Beta Twin", "WR", "NYJ", 1),
            roster("00-3", "Exact Guy", "TE", "NYJ", 1),
        ])
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"WR\",\"Gamma\",\"\",\"Twin\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n\
             \"1-300\",\"TE\",\"Exact\",\"\",\"Guy\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let by_id = |id: &str| plan.rows.iter().find(|r| r.fd_player_id == id).unwrap();
        assert_eq!(
            by_id("200").suggested_gsis_player_id,
            None,
            "two Twins at WR"
        );
        assert_eq!(by_id("300").state, RowState::Matched);
        assert_eq!(
            by_id("300").suggested_gsis_player_id,
            None,
            "a match needs no suggestion"
        );
    }

    /// The roster is scoped to the slate's own week, so a player on the team in
    /// a different week must not match — that is the trade case this whole
    /// dataset exists for.
    #[tokio::test]
    async fn plan_does_not_match_a_roster_entry_from_another_week() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster("00-4040", "Later Signing", "WR", "NYJ", 5)])
            .await
            .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"WR\",\"Later\",\"\",\"Signing\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        assert_eq!(plan.rows[0].state, RowState::Unmatched);
    }

    /// Two players of the same name on one roster: the FanDuel position settles
    /// it. Without a unique answer the row stays unmatched for a human.
    #[tokio::test]
    async fn plan_breaks_a_same_name_roster_tie_on_position_and_gives_up_otherwise() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        nfl.seed_weekly_roster_for_test(&[
            roster("00-1", "Same Name", "WR", "NYJ", 1),
            roster("00-2", "Same Name", "RB", "NYJ", 1),
            roster("00-3", "Twin Guy", "TE", "NYJ", 1),
            roster("00-4", "Twin Guy", "TE", "NYJ", 1),
        ])
        .await
        .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"RB\",\"Same\",\"\",\"Name\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n\
             \"1-300\",\"TE\",\"Twin\",\"\",\"Guy\",\"0\",\"0\",\"4000\",\"BUF@NYJ\",\"NYJ\",\"BUF\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let by_id = |id: &str| plan.rows.iter().find(|r| r.fd_player_id == id).unwrap();
        assert_eq!(
            by_id("200").state,
            RowState::Matched,
            "position broke the tie"
        );
        assert_eq!(by_id("200").gsis_player_id.as_deref(), Some("00-2"));
        assert_eq!(
            by_id("300").state,
            RowState::Unmatched,
            "two TEs of one name is not guessable"
        );
    }

    /// `players` stays authoritative when it has an answer — the roster is a
    /// fallback, not a replacement.
    #[tokio::test]
    async fn plan_prefers_the_player_table_over_the_roster() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(
            &[player("00-players", "Josh Allen", "BUF")],
            &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")],
        )
        .await
        .unwrap();
        nfl.seed_weekly_roster_for_test(&[roster("00-roster", "Josh Allen", "QB", "BUF", 1)])
            .await
            .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-100\",\"QB\",\"Josh\",\"\",\"Allen\",\"0\",\"0\",\"8000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        assert_eq!(plan.rows[0].gsis_player_id.as_deref(), Some("00-players"));
    }

    #[tokio::test]
    async fn plan_prefers_crosswalk_over_name() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        // Crosswalk maps FanDuel id 200 -> gsis 00-9 even though the name won't match.
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            crate::nfl_players::store::upsert(&mut *conn, "00-9", "200").await?;
            Ok(())
        })
        .await
        .unwrap();
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();

        let csv = format!(
            "{HEADER}\n\
             \"1-200\",\"WR\",\"Cryptic\",\"\",\"Name\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        let row = &plan.rows[0];
        assert_eq!(row.state, RowState::Matched);
        assert_eq!(row.gsis_player_id.as_deref(), Some("00-9"));
    }

    #[tokio::test]
    async fn plan_collects_offending_rows_without_erroring() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        nfl.seed_for_test(&[], &[game("2025_01_BUF_NYJ", 1, "BUF", "NYJ")])
            .await
            .unwrap();
        let csv = format!(
            "{HEADER}\n\
             \"1-400\",\"K\",\"Bad\",\"\",\"Position\",\"0\",\"0\",\"5000\",\"BUF@NYJ\",\"BUF\",\"NYJ\",\"\",\"\"\n"
        );
        let plan = super::plan(&db, &nfl, csv.as_bytes(), 2025)
            .await
            .expect("plan");
        assert!(plan.rows.is_empty());
        assert_eq!(plan.offending.len(), 1);
    }

    #[tokio::test]
    async fn plan_errors_on_empty_csv() {
        let db = crate::Db::test(crate::tests::utils::in_memory_pool().await);
        let nfl = nfl_data::NflData::in_memory().await.unwrap();
        let err = super::plan(&db, &nfl, HEADER.as_bytes(), 2025)
            .await
            .unwrap_err();
        assert!(matches!(err, super::PlanError::Empty));
    }

    #[sqlx::test]
    async fn commit_staged_promotes_and_marks_committed(pool: sqlx::SqlitePool) {
        use crate::admin::salary_imports::store as import_store;
        use crate::player_salaries::store as salary_store;

        let db = crate::Db::test(pool.clone());
        let plan = SalaryPlan {
            season: 2025,
            week: Some(1),
            games: 1,
            rows: vec![PlannedRow {
                fd_player_id: "200".into(),
                fd_name: "Resolved Guy".into(),
                fd_team: "BUF".into(),
                dfs_position: DfsPosition::Wr,
                salary: 5000,
                gsis_game_id: "2025_01_BUF_NYJ".into(),
                gsis_player_id: None,
                state: RowState::Unmatched,
                suggested_gsis_player_id: None,
            }],
            offending: vec![],
        };
        let id = import_store::stage(&db, &plan, None).await.unwrap();
        let row_id = import_store::rows_for(&pool, id).await.unwrap()[0].id;
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            import_store::resolve_row(&mut *conn, id, row_id, Some("00-42")).await
        })
        .await
        .unwrap();

        let report = super::commit_staged(&db, id).await.expect("commit");
        assert_eq!(report.salaries, 1);

        // Salary landed.
        let salaries = salary_store::for_games(&pool, &["2025_01_BUF_NYJ"])
            .await
            .unwrap();
        assert_eq!(salaries.len(), 1);
        // Crosswalk written for the resolved player.
        let xw = crate::nfl_players::store::by_fd_id(&pool, "200")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(xw.gsis_player_id, "00-42");
        // Batch committed.
        assert_eq!(
            import_store::get(&pool, id).await.unwrap().unwrap().status,
            "committed"
        );
    }
}
