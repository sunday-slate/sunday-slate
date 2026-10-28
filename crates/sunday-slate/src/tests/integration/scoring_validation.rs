//! Full-season scoring validation: recompute every rostered FanDuel slot from
//! nfl-data and compare to FanDuel's known-good number.
//!
//! Requires a synced `storage/nflverse-cache.db` (run `just nfl-sync`). Skips cleanly
//! when it is absent, so CI without a sync stays green.
//!
//! Every FanDuel scoring category is now derived from nflverse, so nothing is
//! quarantined; each slot must match FanDuel exactly except the explicit
//! `KNOWN_IRREDUCIBLE` allow-list.

use std::collections::HashMap;
use std::path::Path;

use nfl_data::{
    NflData, NflDataConfig, PlayerWeekStats as NflPlayerWeekStats, Season, SeasonType, SeasonWeek,
    TeamWeekStats as NflTeamWeekStats, Week,
};

use crate::Db;
use crate::admin::salary_imports as import;
use crate::nfl_players::store as player_store;
use crate::scoring;
use crate::tests::utils::in_memory_pool;

const SEASON: u16 = 2025;
// Every FanDuel scoring category is now derived from nflverse: the D/ST ones
// from play-by-play, and FU/TD (offensive fumble-recovery TD) from the player
// feed's fumble_recovery_tds. Nothing is quarantined.
const UNCOVERED: [&str; 0] = [];

fn salary_fixture_season_week(filename: &str) -> (SeasonType, u8) {
    if let Some(week) = filename.strip_prefix("week-") {
        (SeasonType::Reg, week.parse::<u8>().unwrap())
    } else {
        match filename {
            "wild-card" => (SeasonType::Post, 19),
            "divisional-round" => (SeasonType::Post, 20),
            "championship-round" => (SeasonType::Post, 21),
            "thanksgiving" => (SeasonType::Reg, 13),
            "christmas-day" => (SeasonType::Reg, 17),
            other => panic!("unknown salary fixture week: {other}"),
        }
    }
}

#[test]
fn salary_fixture_filenames_map_to_complete_season_weeks() {
    let expected = [
        ("week-1", SeasonType::Reg, 1),
        ("week-2", SeasonType::Reg, 2),
        ("week-3", SeasonType::Reg, 3),
        ("week-4", SeasonType::Reg, 4),
        ("week-5", SeasonType::Reg, 5),
        ("week-6", SeasonType::Reg, 6),
        ("week-7", SeasonType::Reg, 7),
        ("week-8", SeasonType::Reg, 8),
        ("week-9", SeasonType::Reg, 9),
        ("week-10", SeasonType::Reg, 10),
        ("week-11", SeasonType::Reg, 11),
        ("week-12", SeasonType::Reg, 12),
        ("week-13", SeasonType::Reg, 13),
        ("week-14", SeasonType::Reg, 14),
        ("week-15", SeasonType::Reg, 15),
        ("week-16", SeasonType::Reg, 16),
        ("week-17", SeasonType::Reg, 17),
        ("week-18", SeasonType::Reg, 18),
        ("wild-card", SeasonType::Post, 19),
        ("divisional-round", SeasonType::Post, 20),
        ("championship-round", SeasonType::Post, 21),
        ("thanksgiving", SeasonType::Reg, 13),
        ("christmas-day", SeasonType::Reg, 17),
    ];

    for (filename, season_type, week) in expected {
        assert_eq!(
            salary_fixture_season_week(filename),
            (season_type, week),
            "{filename}"
        );
    }
}

fn touches_uncovered(stats: &str) -> bool {
    stats
        .split(';')
        .filter_map(|s| s.split(':').next())
        .any(|abbr| UNCOVERED.contains(&abbr))
}

#[tokio::test]
async fn full_season_scores_match_fanduel() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let nfl_db = format!("{manifest}/../../storage/nflverse-cache.db");
    if !Path::new(&nfl_db).exists() {
        eprintln!("skipping: {nfl_db} not present (run `just nfl-sync`)");
        return;
    }
    let fixtures = format!("{manifest}/src/tests/fixtures/fanduel");

    let db = Db::test(in_memory_pool().await);
    let nfl = NflData::connect(NflDataConfig {
        nflverse_database_url: format!("sqlite://{nfl_db}"),
        fanduel_database_url: "sqlite::memory:".into(),
        database_url: "sqlite::memory:".into(),
        ..Default::default()
    })
    .await
    .unwrap();

    // Import every salary fixture. The test is the loader: it opens each file and
    // passes a reader into `import`.
    let mut paths: Vec<_> = std::fs::read_dir(format!("{fixtures}/salaries"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("csv"))
        .collect();
    paths.sort();
    for path in &paths {
        let filename = path.file_stem().and_then(|s| s.to_str()).unwrap();
        let (season_type, week) = salary_fixture_season_week(filename);
        let season_week = SeasonWeek {
            season: Season(SEASON),
            season_type,
            week: Week(week),
        };
        let bytes = std::fs::read(path).unwrap();
        import::import_salary_csv(&db, &nfl, season_week, &bytes)
            .await
            .unwrap_or_else(|e| panic!("import {}: {e}", path.display()));
    }

    let mut pstats: HashMap<u8, Vec<NflPlayerWeekStats>> = HashMap::new();
    let mut tstats: HashMap<u8, Vec<NflTeamWeekStats>> = HashMap::new();

    let mut checked = 0usize;
    let mut quarantined = 0usize;
    let mut unmatched: Vec<String> = Vec::new();
    // (key, message) where key = "{week}|{fd_player_id}" — the stable slot
    // identity the allow-list is checked against.
    let mut mismatches: Vec<(String, String)> = Vec::new();

    let known_good = std::fs::File::open(format!("{fixtures}/known_good.csv")).unwrap();
    let mut rdr = csv::Reader::from_reader(known_good);
    for rec in rdr.records() {
        let rec = rec.unwrap();
        let week: u8 = rec[0].parse().unwrap();
        let fd_player_id: u32 = rec[1].parse().unwrap();
        let roster_position = &rec[2];
        let team = &rec[3];
        let fd_points: f64 = rec[4].parse().unwrap();
        let stats = &rec[5];

        if touches_uncovered(stats) {
            quarantined += 1;
            continue;
        }

        let is_dst = roster_position == "DEF";
        let our: Option<f64> = if is_dst {
            if let std::collections::hash_map::Entry::Vacant(e) = tstats.entry(week) {
                e.insert(
                    nfl.team_week_stats(Season(SEASON), Week(week))
                        .await
                        .unwrap(),
                );
            }
            tstats[&week]
                .iter()
                .find(|t| t.team.as_str() == team)
                .map(|t| scoring::score_defense(t).total)
        } else {
            match player_store::by_fd_id(db.reader(), fd_player_id)
                .await
                .unwrap()
            {
                None => None,
                Some(x) => {
                    if let std::collections::hash_map::Entry::Vacant(e) = pstats.entry(week) {
                        e.insert(
                            nfl.player_week_stats(Season(SEASON), Week(week))
                                .await
                                .unwrap(),
                        );
                    }
                    Some(
                        pstats[&week]
                            .iter()
                            .find(|s| s.gsis_id == x.gsis_player_id)
                            .map(|s| scoring::score_player(s).total)
                            .unwrap_or(0.0),
                    )
                }
            }
        };

        match our {
            None => unmatched.push(format!(
                "week {week} {roster_position} {team} fd={fd_player_id}"
            )),
            Some(total) => {
                checked += 1;
                if (total - fd_points).abs() > 1e-6 {
                    let key = format!("{week}|{fd_player_id}");
                    let msg = format!(
                        "week {week} {roster_position} {team} fd={fd_player_id}: ours={total} fd={fd_points} [{stats}]"
                    );
                    mismatches.push((key, msg));
                }
            }
        }
    }

    eprintln!(
        "checked={checked} quarantined={quarantined} unmatched={} mismatches={}",
        unmatched.len(),
        mismatches.len(),
    );
    for (_, m) in &mismatches {
        eprintln!("  MISMATCH {m}");
    }

    assert!(checked > 1000, "too few slots checked ({checked})");

    // Every scored slot must reproduce FanDuel EXACTLY, except this explicit
    // allow-list of slots we provably cannot reproduce from nflverse data. No
    // count tolerances: a fix must DELETE its entry here, and any new divergence
    // (a regression) fails the test. Keyed by "{week}|{fd_player_id}".
    // Root causes: 2026-07-31-nflverse-vs-fanduel-stat-reconciliation
    const KNOWN_IRREDUCIBLE: &[(&str, &str)] = &[(
        "15|151789",
        "WR nflverse 6 rec/58 yds vs FanDuel 7 rec/69 yds",
    )];
    let allowed: std::collections::HashSet<&str> =
        KNOWN_IRREDUCIBLE.iter().map(|(k, _)| *k).collect();
    let seen: std::collections::HashSet<&str> =
        mismatches.iter().map(|(k, _)| k.as_str()).collect();

    let unexpected: Vec<&String> = mismatches
        .iter()
        .filter(|(k, _)| !allowed.contains(k.as_str()))
        .map(|(_, m)| m)
        .collect();
    assert!(
        unexpected.is_empty(),
        "scoring diverged in slots not on the known-irreducible allow-list \
         (a regression — or a slot whose data changed):\n{}",
        unexpected
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );

    let fixed: Vec<&str> = KNOWN_IRREDUCIBLE
        .iter()
        .filter(|(k, _)| !seen.contains(k))
        .map(|(k, _)| *k)
        .collect();
    assert!(
        fixed.is_empty(),
        "these allow-listed slots now match FanDuel — delete them from \
         KNOWN_IRREDUCIBLE:\n{}",
        fixed.join("\n")
    );
}
