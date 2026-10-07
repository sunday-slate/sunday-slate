pub(crate) mod pbp;
pub(crate) mod player_stats;
pub(crate) mod players;
pub(crate) mod schedules;
pub(crate) mod weekly_rosters;

use serde::de::DeserializeOwned;

use crate::error::NflDataError;
use crate::model::SeasonType;

/// Strict CSV parse: any undeserializable or unconvertible row fails the whole
/// asset (a silently dropped row would be a player quietly missing from
/// scoring). `convert` returns Ok(None) to skip rows outside the season window.
pub(crate) fn parse_csv<Raw, T, F>(
    asset: &str,
    bytes: &[u8],
    convert: F,
) -> Result<Vec<T>, NflDataError>
where
    Raw: DeserializeOwned,
    F: Fn(Raw) -> Result<Option<T>, String>,
{
    let mut reader = csv::Reader::from_reader(bytes);
    let mut out = Vec::new();
    for (i, record) in reader.deserialize::<Raw>().enumerate() {
        // 1-based file line: +1 for the header, +1 for zero-indexing.
        let row = (i + 2) as u64;
        let raw = record.map_err(|e| NflDataError::Parse {
            asset: asset.to_string(),
            row,
            source: Box::new(e),
        })?;
        if let Some(value) = convert(raw).map_err(|msg| NflDataError::Parse {
            asset: asset.to_string(),
            row,
            source: msg.into(),
        })? {
            out.push(value);
        }
    }
    Ok(out)
}
pub(crate) fn optional_espn_id(value: String) -> Result<Option<String>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().all(|character| character.is_ascii_digit()) {
        return Ok(Some(value));
    }
    Err(format!("invalid espn_id {value:?}"))
}

pub(crate) fn parse_season_type(s: &str) -> Result<SeasonType, String> {
    match s {
        "REG" => Ok(SeasonType::Reg),
        "POST" | "WC" | "DIV" | "CON" | "SB" => Ok(SeasonType::Post),
        other => Err(format!("unknown season/game type {other:?}")),
    }
}

/// Counting stats: upstream writes numeric NA as an empty cell, which means 0.
/// Garbage (non-numeric text) still fails the row via serde before this runs.
pub(crate) fn count(v: Option<f64>) -> u32 {
    v.unwrap_or(0.0).round() as u32
}

/// Yardage can be negative.
pub(crate) fn yards(v: Option<f64>) -> i32 {
    v.unwrap_or(0.0).round() as i32
}
