use crate::player_salaries::model::DfsPosition;

/// Lifecycle of one staged salary row. `Matched`/`Dst` auto-resolved at upload;
/// `Unmatched` needs an admin decision; `Resolved`/`Skipped` are the two decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum RowState {
    Matched,
    Dst,
    Unmatched,
    Resolved,
    Skipped,
}

/// A persisted upload batch.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SalaryImport {
    pub id: i64,
    pub season: i64,
    pub week: Option<i64>,
    pub status: String, // "pending" | "committed"
    pub games: i64,
    pub created_by: Option<i64>,
    pub created_at: String,
    pub committed_at: Option<String>,
}

/// One staged salary row within a batch.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SalaryImportRow {
    pub id: i64,
    pub import_id: i64,
    pub fd_player_id: String,
    pub fd_name: String,
    pub fd_team: String,
    pub dfs_position: DfsPosition,
    pub salary: i64,
    pub gsis_game_id: String,
    pub gsis_player_id: Option<String>,
    pub state: RowState,
    pub suggested_gsis_player_id: Option<String>,
}

/// The three states an admin can act on. Auto-matched rows (`Matched`, `Dst`)
/// have no group — they are never rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowGroup {
    #[default]
    Unmatched,
    Resolved,
    Skipped,
}

impl RowGroup {
    pub const ALL: [RowGroup; 3] = [RowGroup::Unmatched, RowGroup::Resolved, RowGroup::Skipped];

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "unmatched" => Some(RowGroup::Unmatched),
            "resolved" => Some(RowGroup::Resolved),
            "skipped" => Some(RowGroup::Skipped),
            _ => None,
        }
    }

    /// Path segment under the import, so every group is its own address. The
    /// default group has none — it lives on the bare import URL.
    pub fn path_suffix(self) -> &'static str {
        match self {
            RowGroup::Unmatched => "",
            RowGroup::Resolved => "/resolved",
            RowGroup::Skipped => "/skipped",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            RowGroup::Unmatched => "unmatched",
            RowGroup::Resolved => "resolved",
            RowGroup::Skipped => "skipped",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RowGroup::Unmatched => "Needs a decision",
            RowGroup::Resolved => "Resolved",
            RowGroup::Skipped => "Skipped",
        }
    }

    pub fn state(self) -> RowState {
        match self {
            RowGroup::Unmatched => RowState::Unmatched,
            RowGroup::Resolved => RowState::Resolved,
            RowGroup::Skipped => RowState::Skipped,
        }
    }

    pub fn empty_message(self) -> &'static str {
        match self {
            RowGroup::Unmatched => "Every row has a decision.",
            RowGroup::Resolved => "No rows resolved yet.",
            RowGroup::Skipped => "No rows skipped.",
        }
    }
}
