use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

use nfl_data::LiveSlate;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use tokio::sync::mpsc::Receiver;

use crate::poll::{
    CompletionReason, EVERY_FIFTH_PLAY_BY_PLAY, FINAL_FOLLOWUP_INTERVAL_SECONDS, FINAL_FOLLOWUPS,
    POLL_INTERVAL_SECONDS, PollEvent, safe_provider_game_id,
};

pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("capture I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("capture JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid capture: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureReport {
    pub event_count: u64,
    pub completed: bool,
    pub completion_reason: Option<CompletionReason>,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub schema_version: u32,
    pub created_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub slate_date: Date,
    pub games: Vec<ManifestGame>,
    pub policy: ManifestPolicy,
    pub completion_reason: Option<CompletionReason>,
    pub terminal_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManifestGame {
    pub gsis_game_id: String,
    pub provider_game_id: String,
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManifestPolicy {
    pub scoreboard_cadence_seconds: u64,
    pub box_score_cadence_seconds: u64,
    pub every_fifth_play_by_play: bool,
    pub final_followups: u32,
    pub final_followup_interval_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LineEnvelope {
    pub schema_version: u32,
    pub event: PollEvent,
}

pub struct CaptureWriter {
    root: PathBuf,
    manifest: Manifest,
    scoreboard: BufWriter<File>,
    games: BTreeMap<String, BufWriter<File>>,
    event_count: u64,
    terminal: Option<(u64, CompletionReason)>,
}

impl CaptureWriter {
    pub fn create(root: impl AsRef<Path>, slate: &LiveSlate) -> Result<Self, CaptureError> {
        if slate.games.is_empty() {
            return Err(CaptureError::Invalid("slate has no games".into()));
        }
        let root = root.as_ref().to_path_buf();
        if root.exists() {
            if !root.is_dir() {
                return Err(CaptureError::Invalid(format!(
                    "capture root {:?} is not a directory",
                    root
                )));
            }
            if fs::read_dir(&root)?.next().is_some() {
                return Err(CaptureError::Invalid(
                    "capture root must be absent or empty".into(),
                ));
            }
        } else {
            fs::create_dir_all(&root)?;
        }
        let games_root = root.join("games");
        fs::create_dir_all(&games_root)?;

        let mut gsis_ids = BTreeSet::new();
        let mut provider_ids = BTreeSet::new();
        let mut safe_names = BTreeSet::new();
        let mut manifest_games = Vec::with_capacity(slate.games.len());
        for game in &slate.games {
            if game.gsis_game_id.is_empty() || game.provider_game_id.is_empty() {
                return Err(CaptureError::Invalid(
                    "capture game ids must not be empty".into(),
                ));
            }
            if !gsis_ids.insert(&game.gsis_game_id) {
                return Err(CaptureError::Invalid(format!(
                    "duplicate GSIS game id {:?}",
                    game.gsis_game_id
                )));
            }
            if !provider_ids.insert(&game.provider_game_id) {
                return Err(CaptureError::Invalid(format!(
                    "duplicate provider game id {:?}",
                    game.provider_game_id
                )));
            }
            let safe_name = safe_provider_game_id(&game.provider_game_id)
                .map_err(|error| CaptureError::Invalid(error.to_string()))?;
            if !safe_names.insert(safe_name.clone()) {
                return Err(CaptureError::Invalid(format!(
                    "provider game id {:?} collides as {safe_name:?}",
                    game.provider_game_id
                )));
            }
            manifest_games.push(ManifestGame {
                gsis_game_id: game.gsis_game_id.clone(),
                provider_game_id: game.provider_game_id.clone(),
                file_name: game_file_name(&safe_name),
            });
        }

        let scoreboard_path = root.join("scoreboard.ndjson");
        let scoreboard = create_new_buffer(&scoreboard_path)?;
        let mut games = BTreeMap::new();
        for game in &manifest_games {
            let path = root.join(&game.file_name);
            games.insert(game.provider_game_id.clone(), create_new_buffer(&path)?);
        }

        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            created_at: OffsetDateTime::now_utc(),
            completed_at: None,
            slate_date: slate.date,
            games: manifest_games,
            policy: ManifestPolicy {
                scoreboard_cadence_seconds: POLL_INTERVAL_SECONDS,
                box_score_cadence_seconds: POLL_INTERVAL_SECONDS,
                every_fifth_play_by_play: EVERY_FIFTH_PLAY_BY_PLAY,
                final_followups: FINAL_FOLLOWUPS,
                final_followup_interval_seconds: FINAL_FOLLOWUP_INTERVAL_SECONDS,
            },
            completion_reason: None,
            terminal_sequence: None,
        };
        write_manifest(&root, &manifest)?;

        Ok(Self {
            root,
            manifest,
            scoreboard,
            games,
            event_count: 0,
            terminal: None,
        })
    }

    pub async fn consume(
        mut self,
        mut receiver: Receiver<PollEvent>,
    ) -> Result<CaptureReport, CaptureError> {
        while let Some(event) = receiver.recv().await {
            self.consume_event(event)?;
        }
        self.flush_streams()?;
        Ok(self.report())
    }

    fn consume_event(&mut self, event: PollEvent) -> Result<(), CaptureError> {
        if self.terminal.is_some() {
            return Err(CaptureError::Invalid(
                "poll event arrived after terminal event".into(),
            ));
        }
        match &event {
            PollEvent::Scoreboard { .. } => {
                let envelope = LineEnvelope {
                    schema_version: SCHEMA_VERSION,
                    event,
                };
                write_line(&mut self.scoreboard, &envelope)?;
                self.event_count += 1;
            }
            PollEvent::BoxScore { game, .. } => {
                let file = self.games.get_mut(&game.provider_game_id).ok_or_else(|| {
                    CaptureError::Invalid(format!(
                        "box event references unknown provider game {:?}",
                        game.provider_game_id
                    ))
                })?;
                let envelope = LineEnvelope {
                    schema_version: SCHEMA_VERSION,
                    event,
                };
                write_line(file, &envelope)?;
                self.event_count += 1;
            }
            PollEvent::RunFinished { sequence, reason } => {
                self.flush_streams()?;
                self.terminal = Some((*sequence, *reason));
                self.manifest.completed_at = Some(OffsetDateTime::now_utc());
                self.manifest.completion_reason = Some(*reason);
                self.manifest.terminal_sequence = Some(*sequence);
                write_manifest(&self.root, &self.manifest)?;
            }
        }
        Ok(())
    }

    fn flush_streams(&mut self) -> Result<(), CaptureError> {
        self.scoreboard.flush()?;
        for file in self.games.values_mut() {
            file.flush()?;
        }
        Ok(())
    }

    fn report(&self) -> CaptureReport {
        CaptureReport {
            event_count: self.event_count,
            completed: self.terminal.is_some(),
            completion_reason: self.terminal.map(|(_, reason)| reason),
            manifest_path: self.root.join("manifest.json"),
        }
    }
}
pub(crate) fn game_file_name(safe_name: &str) -> String {
    format!("games/{safe_name}.ndjson")
}

fn create_new_buffer(path: &Path) -> Result<BufWriter<File>, CaptureError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    Ok(BufWriter::new(file))
}

fn write_line<T: Serialize>(file: &mut BufWriter<File>, value: &T) -> Result<(), CaptureError> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub(crate) fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), CaptureError> {
    let temporary = root.join(".manifest.json.tmp");
    {
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, manifest)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(temporary, root.join("manifest.json"))?;
    Ok(())
}

pub(crate) fn read_manifest(path: &Path) -> Result<Manifest, CaptureError> {
    let bytes = fs::read(path.join("manifest.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::poll::PollRequest;
    use nfl_data::{
        LiveGame, LiveScoreboard, ProviderOutcome, ProviderResponse, RawBody, Season, SeasonType,
        TeamAbbr, Week,
    };
    use time::macros::{date, datetime};

    fn game() -> LiveGame {
        LiveGame {
            gsis_game_id: "2026_01_SF_LAR".into(),
            provider_game_id: "20260910_SF@LAR".into(),
            season: Season(2026),
            week: Week(1),
            season_type: SeasonType::Reg,
            kickoff: Some(datetime!(2026-09-10 20:35:00 -04:00)),
            home_team: TeamAbbr("LAR".into()),
            away_team: TeamAbbr("SF".into()),
        }
    }

    fn scoreboard_event(sequence: u64) -> PollEvent {
        let now = OffsetDateTime::now_utc();
        PollEvent::Scoreboard {
            sequence,
            request: PollRequest {
                endpoint: "getNFLScoresOnly".into(),
                query: BTreeMap::new(),
            },
            response: ProviderResponse {
                requested_at: now,
                received_at: now,
                elapsed_ms: 1,
                http_status: Some(200),
                headers: BTreeMap::new(),
                raw_body: Some(RawBody::Json(serde_json::json!({"games": []}))),
                outcome: ProviderOutcome::Value(LiveScoreboard { games: vec![] }),
            },
        }
    }

    #[tokio::test]
    async fn creates_layout_and_finalizes_manifest_only_on_terminal_event() {
        let directory = tempfile::tempdir().unwrap();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };
        let writer = CaptureWriter::create(directory.path(), &slate).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        sender.send(scoreboard_event(1)).await.unwrap();
        sender
            .send(PollEvent::RunFinished {
                sequence: 2,
                reason: CompletionReason::FinalFollowupsComplete,
            })
            .await
            .unwrap();
        drop(sender);
        let report = writer.consume(receiver).await.unwrap();
        assert!(report.completed);
        assert!(directory.path().join("manifest.json").exists());
        assert!(directory.path().join("scoreboard.ndjson").exists());
        assert!(
            directory
                .path()
                .join("games/20260910_SF_at_LAR.ndjson")
                .exists()
        );
        assert!(!directory.path().join("polls.ndjson").exists());
        let manifest = read_manifest(directory.path()).unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.policy.final_followups, 3);
        assert_eq!(manifest.policy.final_followup_interval_seconds, 300);
        assert_eq!(manifest.terminal_sequence, Some(2));
        let lines = fs::read_to_string(directory.path().join("scoreboard.ndjson")).unwrap();
        assert!(lines.contains("schema_version"));
        assert!(!lines.contains("RunFinished"));
    }

    #[tokio::test]
    async fn incomplete_receiver_close_leaves_manifest_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };
        let writer = CaptureWriter::create(directory.path(), &slate).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender.send(scoreboard_event(1)).await.unwrap();
        drop(sender);
        let report = writer.consume(receiver).await.unwrap();
        assert!(!report.completed);
        let manifest = read_manifest(directory.path()).unwrap();
        assert_eq!(manifest.completed_at, None);
        assert_eq!(manifest.completion_reason, None);
    }

    #[test]
    fn refuses_to_reuse_a_nonempty_capture_root() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("stale.ndjson"), "stale\n").unwrap();
        let slate = LiveSlate {
            date: date!(2026 - 09 - 10),
            games: vec![game()],
        };

        assert!(matches!(
            CaptureWriter::create(directory.path(), &slate),
            Err(CaptureError::Invalid(message)) if message.contains("absent or empty")
        ));
    }
}
