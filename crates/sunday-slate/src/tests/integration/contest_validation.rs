//! Golden validation: our `resolve` rules reproduce FanDuel's 2025 contest
//! slates. 22 of 23 match exactly; Wild Card is the one intentional deviation —
//! our Saturday+Sunday playoff rule drops the Monday night game (HOU@PIT).
//!
//! Requires a synced `storage/nflverse-data.db` (run `just nfl-sync`). Skips cleanly
//! when it is absent, so CI without a sync stays green.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use nfl_data::{NflData, NflDataConfig, Season};

use crate::contests::rule::{Rule, resolve};

const SEASON: u16 = 2025;

fn load_fanduel(path: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut rdr = csv::Reader::from_path(path).expect("open fixture");
    for rec in rdr.records() {
        let rec = rec.expect("row");
        let (name, away, home) = (&rec[0], &rec[1], &rec[2]);
        map.entry(name.to_string())
            .or_default()
            .insert(format!("{away}@{home}"));
    }
    map
}

#[tokio::test]
async fn resolve_matches_fanduel_2025() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let nfl_db = format!("{manifest}/../../storage/nflverse-data.db");
    if !Path::new(&nfl_db).exists() {
        eprintln!("skipping: {nfl_db} not present (run `just nfl-sync`)");
        return;
    }
    let fixture = format!("{manifest}/src/tests/fixtures/contests/fanduel-2025.csv");
    let fanduel = load_fanduel(&fixture);
    assert_eq!(fanduel.len(), 23, "expected 23 FanDuel contests");

    let nfl = NflData::connect(NflDataConfig {
        database_url: format!("sqlite://{nfl_db}"),
        ..Default::default()
    })
    .await
    .unwrap();
    let games = nfl.games(Season(SEASON)).await.unwrap();

    for (name, expected) in &fanduel {
        let rule = Rule::from_name(name).unwrap_or_else(|| panic!("unknown contest {name}"));
        let got: BTreeSet<String> = resolve(&rule, SEASON, &games)
            .iter()
            .map(|g| format!("{}@{}", g.away_team.0, g.home_team.0))
            .collect();

        // Wild Card is the single documented deviation: FanDuel kept the Monday
        // game HOU@PIT; our Sat+Sun rule drops it.
        let expected: BTreeSet<String> = if name == "Wild Card" {
            expected
                .iter()
                .filter(|m| *m != "HOU@PIT")
                .cloned()
                .collect()
        } else {
            expected.clone()
        };

        assert_eq!(&got, &expected, "slate mismatch for {name}");
    }
}
