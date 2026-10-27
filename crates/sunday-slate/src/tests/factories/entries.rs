//! Contest, entry, and NFL stat fixtures for scoring-path tests.

use nfl_data::{
    Game, Player, PlayerIdentity, PlayerWeekStats as NflPlayerWeekStats, Season, SeasonType,
    TeamAbbr, TeamWeekStats as NflTeamWeekStats, Week, WeeklyRosterEntry,
};
use sqlx::SqlitePool;

/// A KC home game against BUF, kickoff unknown. Set `kickoff` through struct
/// update syntax when the test needs one.
pub fn game(id: &str, week: u8) -> Game {
    Game {
        gsis_game_id: id.into(),
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        kickoff: None,
        home_team: TeamAbbr::KC,
        away_team: TeamAbbr::BUF,
        home_score: None,
        away_score: None,
    }
}

/// A KC home game against BUF kicking off at `kickoff`.
pub fn game_at(id: &str, week: u8, kickoff: time::OffsetDateTime) -> Game {
    Game {
        kickoff: Some(kickoff),
        ..game(id, week)
    }
}

/// A KC player with every stat at zero. Set the stats the test cares about
/// through struct update syntax.
pub fn player_stats(gsis: &str, week: u8) -> NflPlayerWeekStats {
    NflPlayerWeekStats {
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        gsis_id: gsis.into(),
        team: TeamAbbr::KC,
        opponent: None,
        completions: 0,
        attempts: 0,
        passing_yards: 0,
        passing_tds: 0,
        passing_interceptions: 0,
        rushing_attempts: 0,
        rushing_yards: 0,
        rushing_tds: 0,
        targets: 0,
        receptions: 0,
        receiving_yards: 0,
        receiving_tds: 0,
        fumbles_lost: 0,
        two_point_conversions: 0,
        special_teams_tds: 0,
        fumble_recovery_tds: 0,
    }
}

pub fn rb_stats(gsis: &str, week: u8, rush_yards: i32) -> NflPlayerWeekStats {
    NflPlayerWeekStats {
        rushing_yards: rush_yards,
        ..player_stats(gsis, week)
    }
}

/// A team defense with every stat at zero, including points allowed.
pub fn defense_stats(team: &str, opponent: &str, week: u8) -> NflTeamWeekStats {
    NflTeamWeekStats {
        season: Season(2025),
        week: Week(week),
        season_type: SeasonType::Reg,
        team: team.parse().unwrap(),
        opponent: opponent.parse().unwrap(),
        gsis_game_id: format!("2025_{week:02}_{opponent}_{team}"),
        sacks: 0,
        interceptions: 0,
        fumble_recoveries: 0,
        safeties: 0,
        touchdowns: 0,
        blocked_kicks: 0,
        conversion_returns: 0,
        points_allowed: 0,
    }
}

pub fn scoreless_defense_stats(team: &str, opponent: &str, week: u8) -> NflTeamWeekStats {
    NflTeamWeekStats {
        points_allowed: 24,
        ..defense_stats(team, opponent, week)
    }
}

/// A KC player in the `players` release.
pub fn player(gsis: &str, name: &str) -> Player {
    Player {
        gsis_id: gsis.into(),
        espn_id: None,
        full_name: name.into(),
        first_name: None,
        last_name: None,
        position: None,
        latest_team: Some(TeamAbbr::KC),
        headshot_url: None,
    }
}

/// A KC weekly roster entry — how a pick absent from the `players` release
/// still gets a name.
pub fn weekly_roster_entry(gsis: &str, name: &str, week: u8) -> WeeklyRosterEntry {
    WeeklyRosterEntry {
        season: Season(2025),
        week: Week(week),
        team: TeamAbbr::KC,
        gsis_id: Some(gsis.into()),
        espn_id: None,
        full_name: name.into(),
        last_name: None,
        position: None,
    }
}

/// A resolved identity, as `NflData::identify` returns one.
pub fn identity(name: &str, team: Option<&str>) -> PlayerIdentity {
    PlayerIdentity {
        name: name.into(),
        team: team.map(|t| t.parse().unwrap()),
        position: None,
        headshot_url: None,
    }
}

pub async fn contest(pool: &SqlitePool, name: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO contests (name) VALUES (?1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Insert a contest and freeze its slate onto `games`.
pub async fn contest_with_games(pool: &SqlitePool, name: &str, games: &[&str]) -> i64 {
    let id = contest(pool, name).await;
    for game in games {
        sqlx::query("INSERT INTO contest_games (contest_id, gsis_game_id) VALUES (?1, ?2)")
            .bind(id)
            .bind(game)
            .execute(pool)
            .await
            .unwrap();
    }
    id
}

/// Insert a full nine-slot entry and return its id. Only `rb1` is the
/// caller's; the other slots take the filler ids below, so an entry scores
/// exactly its RB1's points.
pub async fn full_entry(pool: &SqlitePool, contest_id: i64, team_id: i64, rb1: &str) -> i64 {
    let entry_id: i64 = sqlx::query_scalar(
        "INSERT INTO entries (contest_id, fantasy_team_id) VALUES (?1, ?2) RETURNING id",
    )
    .bind(contest_id)
    .bind(team_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let players = [
        ("QB", "00-F1"),
        ("RB2", "00-F2"),
        ("WR1", "00-F3"),
        ("WR2", "00-F4"),
        ("WR3", "00-F5"),
        ("TE", "00-F6"),
        ("FLEX", "00-F7"),
    ];
    for (slot, gsis) in players.iter().copied().chain([("RB1", rb1)]) {
        sqlx::query(
            "INSERT INTO entry_slots (entry_id, roster_slot, gsis_player_id) VALUES (?1, ?2, ?3)",
        )
        .bind(entry_id)
        .bind(slot)
        .bind(gsis)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO entry_slots (entry_id, roster_slot, team_abbr) VALUES (?1, 'DEF', 'KC')",
    )
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
    entry_id
}

/// One salary source row to seed through the production CSV import, plus the
/// GSIS identity to confirm for it (skip `gsis_id` for DST rows).
pub struct TestSalary<'a> {
    pub fd_id: u32,
    pub gsis_id: Option<&'a str>,
    pub team: TeamAbbr,
    pub position: nfl_model::DfsPosition,
    pub salary: u32,
}

/// Seed salaries for one game through the real production APIs: the caller
/// seeds the schedule and identities first (this helper must not call
/// `seed_for_test` again — its inserts are not upserts). Builds a full-week
/// CSV from synthetic unique names, imports it with `import_salary_csv`, and
/// confirms each supplied non-DST GSIS link with `resolve_salary_player`.
///
/// Callers assign stable unique FD IDs and may rerun the helper with a new
/// salary amount to update the source price without duplicating nflverse seeds.
pub async fn seed_salaries(nfl: &nfl_data::NflData, game: &Game, rows: &[TestSalary<'_>]) {
    use nfl_model::SeasonWeek;

    let season_week = SeasonWeek {
        season: game.season,
        season_type: game.season_type,
        week: game.week,
    };

    let matchup = format!("{}@{}", game.away_team.as_str(), game.home_team.as_str());
    let mut csv = String::from(
        "Id,Position,First Name,Nickname,Last Name,FPPG,Played,Salary,Game,Team,Opponent,Injury Indicator,Injury Details\n",
    );
    for row in rows {
        let opponent = if row.team == game.away_team {
            game.home_team.as_str()
        } else {
            game.away_team.as_str()
        };
        let first = format!("Fixture{}", row.fd_id);
        let last = format!("Player{}", row.fd_id);
        let position: String = String::from(row.position);
        csv.push_str(&format!(
            "\"1-{fd}\",\"{position}\",\"{first}\",\"\",\"{last}\",\"0\",\"0\",\"{salary}\",\"{matchup}\",\"{team}\",\"{opponent}\",\"\",\"\"\n",
            fd = row.fd_id,
            position = position,
            first = first,
            last = last,
            salary = row.salary,
            matchup = matchup,
            team = row.team.as_str(),
            opponent = opponent,
        ));
    }

    nfl.import_salary_csv(season_week, csv.as_bytes())
        .await
        .expect("seed_salaries import");

    for row in rows {
        let Some(gsis_id) = row.gsis_id else { continue };
        nfl.resolve_salary_player(season_week, row.fd_id, gsis_id)
            .await
            .expect("seed_salaries resolve");
    }
}
