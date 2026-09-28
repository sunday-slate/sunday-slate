use std::collections::BTreeMap;
use std::io::Read;

use crate::client::{GithubClient, Release, ReleaseAsset};
use crate::config::NflDataConfig;
use crate::error::NflDataError;
use crate::ingest;
use crate::model::{Dataset, DatasetReport, DatasetStatus, Season, SyncReport};
use crate::store::sync_state::{self, SyncedAsset};
use crate::store::{self, Store};

pub(crate) async fn run(store: &Store, config: &NflDataConfig) -> Result<SyncReport, NflDataError> {
    let client = GithubClient::new(
        config.github_api_base.clone(),
        config
            .github_token
            .as_ref()
            .map(|token| token.expose().to_string()),
    )?;
    let mut datasets = Vec::with_capacity(Dataset::ALL.len());
    for dataset in Dataset::ALL {
        let status = sync_dataset(store, &client, config, dataset)
            .await
            .unwrap_or_else(DatasetStatus::Failed);
        if let DatasetStatus::Failed(error) = &status {
            tracing::warn!(dataset = dataset.name(), %error, "dataset sync failed");
        }
        datasets.push(DatasetReport { dataset, status });
    }
    Ok(SyncReport { datasets })
}

async fn sync_dataset(
    store: &Store,
    client: &GithubClient,
    config: &NflDataConfig,
    dataset: Dataset,
) -> Result<DatasetStatus, NflDataError> {
    let tag = dataset.release_tag();
    let release = client.release(tag).await?;
    let selected = select_assets(dataset, &release, config.earliest_season)?;

    let mut updated_assets = 0u32;
    let mut total_rows = 0u64;
    let mut first_error: Option<NflDataError> = None;
    for SelectedAsset { asset, season } in selected {
        match sync_one_asset(store, client, config, dataset, asset, season, tag).await {
            Ok(Some(rows)) => {
                updated_assets += 1;
                total_rows += rows;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    dataset = dataset.name(),
                    asset = %asset.name,
                    %error,
                    "asset sync failed; continuing with remaining assets in this dataset"
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    if let Some(error) = first_error {
        tracing::warn!(
            dataset = dataset.name(),
            updated_assets,
            total_rows,
            "dataset sync partially failed: {updated_assets} asset(s) updated ({total_rows} rows) before the failure"
        );
        return Ok(DatasetStatus::Failed(error));
    }

    Ok(if updated_assets == 0 {
        DatasetStatus::Unchanged
    } else {
        DatasetStatus::Updated {
            assets: updated_assets,
            rows: total_rows,
        }
    })
}

/// Downloads, parses and stores a single asset. Returns `Ok(None)` when the
/// asset was already current (skipped), `Ok(Some(rows))` on a successful
/// store, or `Err` on any failure. Errors here are per-asset — the caller
/// isolates them so one bad season doesn't block the others in the same
/// dataset.
async fn sync_one_asset(
    store: &Store,
    client: &GithubClient,
    config: &NflDataConfig,
    dataset: Dataset,
    asset: &ReleaseAsset,
    season: Option<u16>,
    tag: &str,
) -> Result<Option<u64>, NflDataError> {
    if sync_state::is_current(store.reader(), tag, &asset.name, &asset.updated_at).await? {
        return Ok(None);
    }
    let bytes = client.download(&asset.browser_download_url).await?;
    let bytes = maybe_gunzip(&asset.name, bytes)?;
    let synced = SyncedAsset {
        release_tag: tag,
        asset_name: &asset.name,
        updated_at: &asset.updated_at,
    };
    let rows = match dataset {
        Dataset::Schedules => {
            let games = ingest::schedules::parse(&asset.name, &bytes, config.earliest_season)?;
            store::games::replace(store, &synced, &games).await?
        }
        Dataset::Players => {
            let players = ingest::players::parse(&asset.name, &bytes)?;
            store::players::replace(store, &synced, &players).await?
        }
        Dataset::Rosters => {
            let entries = ingest::rosters::parse(&asset.name, &bytes)?;
            let season = season.expect("per-season datasets always carry a season");
            store::rosters::replace(store, &synced, Season(season), &entries).await?
        }
        Dataset::WeeklyRosters => {
            let entries = ingest::weekly_rosters::parse(&asset.name, &bytes)?;
            let season = season.expect("per-season datasets always carry a season");
            store::weekly_rosters::replace(store, &synced, Season(season), &entries).await?
        }
        Dataset::PlayerWeekStats => {
            let stats = ingest::player_stats::parse(&asset.name, &bytes)?;
            let season = season.expect("per-season datasets always carry a season");
            store::player_stats::replace(store, &synced, Season(season), &stats).await?
        }
        Dataset::Pbp => {
            let season = season.expect("per-season datasets always carry a season");
            let stats = ingest::pbp::parse(&asset.name, &bytes)?;
            store::team_stats::replace(store, &synced, Season(season), &stats).await?
        }
    };
    Ok(Some(rows))
}

struct SelectedAsset<'a> {
    asset: &'a ReleaseAsset,
    season: Option<u16>,
}

fn select_assets<'a>(
    dataset: Dataset,
    release: &'a Release,
    earliest_season: u16,
) -> Result<Vec<SelectedAsset<'a>>, NflDataError> {
    match dataset {
        Dataset::Schedules => single_asset(release, dataset, "games"),
        Dataset::Players => single_asset(release, dataset, "players"),
        Dataset::Rosters => Ok(per_season_assets(release, "roster_", earliest_season)),
        Dataset::WeeklyRosters => Ok(per_season_assets(
            release,
            "roster_weekly_",
            earliest_season,
        )),
        Dataset::PlayerWeekStats => Ok(per_season_assets(
            release,
            "stats_player_week_",
            earliest_season,
        )),
        Dataset::Pbp => Ok(per_season_assets(release, "play_by_play_", earliest_season)),
    }
}

/// Single-file datasets: prefer the gzipped asset, fall back to plain csv
/// (the rosters release, for one, publishes no .csv.gz).
fn single_asset<'a>(
    release: &'a Release,
    dataset: Dataset,
    stem: &str,
) -> Result<Vec<SelectedAsset<'a>>, NflDataError> {
    let gz = format!("{stem}.csv.gz");
    let plain = format!("{stem}.csv");
    release
        .assets
        .iter()
        .find(|a| a.name == gz)
        .or_else(|| release.assets.iter().find(|a| a.name == plain))
        .map(|asset| {
            vec![SelectedAsset {
                asset,
                season: None,
            }]
        })
        .ok_or_else(|| NflDataError::MissingAsset {
            release_tag: dataset.release_tag().to_string(),
            name: plain,
        })
}

/// Per-season datasets: whatever seasons exist upstream in the window, one
/// asset each, .csv.gz preferred over .csv. An empty result is legal (e.g. a
/// window entirely in the future).
fn per_season_assets<'a>(
    release: &'a Release,
    prefix: &str,
    earliest_season: u16,
) -> Vec<SelectedAsset<'a>> {
    let mut by_season: BTreeMap<u16, &'a ReleaseAsset> = BTreeMap::new();
    for gz_pass in [false, true] {
        for asset in &release.assets {
            if let Some((season, gzipped)) = season_of(&asset.name, prefix)
                && gzipped == gz_pass
                && season >= earliest_season
            {
                by_season.insert(season, asset);
            }
        }
    }
    by_season
        .into_iter()
        .map(|(season, asset)| SelectedAsset {
            asset,
            season: Some(season),
        })
        .collect()
}

fn season_of(name: &str, prefix: &str) -> Option<(u16, bool)> {
    let rest = name.strip_prefix(prefix)?;
    let (digits, gzipped) = match rest.strip_suffix(".csv.gz") {
        Some(d) => (d, true),
        None => (rest.strip_suffix(".csv")?, false),
    };
    if digits.len() != 4 {
        return None;
    }
    digits.parse().ok().map(|season| (season, gzipped))
}

fn maybe_gunzip(name: &str, bytes: Vec<u8>) -> Result<Vec<u8>, NflDataError> {
    if !name.ends_with(".gz") {
        return Ok(bytes);
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut out)
        .map_err(|source| NflDataError::Gunzip {
            asset: name.to_string(),
            source,
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            browser_download_url: format!("https://example.invalid/{name}"),
        }
    }

    #[test]
    fn per_season_prefers_gz_and_filters_window() {
        let release = Release {
            assets: vec![
                asset("stats_player_week_2023.csv"),
                asset("stats_player_week_2024.csv"),
                asset("stats_player_week_2024.csv.gz"),
                asset("stats_player_week_2025.csv"),
                asset("stats_player_week_2025.parquet"),
                asset("timestamp.json"),
            ],
        };
        let selected = per_season_assets(&release, "stats_player_week_", 2024);
        let names: Vec<_> = selected.iter().map(|s| s.asset.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "stats_player_week_2024.csv.gz",
                "stats_player_week_2025.csv"
            ]
        );
        assert_eq!(selected[0].season, Some(2024));
    }

    #[test]
    fn single_asset_falls_back_to_plain_csv_and_errors_when_absent() {
        let release = Release {
            assets: vec![asset("games.csv"), asset("games.parquet")],
        };
        let selected = single_asset(&release, Dataset::Schedules, "games").unwrap();
        assert_eq!(selected[0].asset.name, "games.csv");

        let empty = Release { assets: vec![] };
        assert!(matches!(
            single_asset(&empty, Dataset::Schedules, "games"),
            Err(NflDataError::MissingAsset { .. })
        ));
    }

    #[test]
    fn gunzip_roundtrips_and_passes_plain_bytes_through() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;

        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(b"a,b\n1,2\n").unwrap();
        let gz = enc.finish().unwrap();

        assert_eq!(maybe_gunzip("x.csv.gz", gz).unwrap(), b"a,b\n1,2\n");
        assert_eq!(maybe_gunzip("x.csv", b"a,b\n".to_vec()).unwrap(), b"a,b\n");
    }
}
