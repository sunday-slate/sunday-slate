use std::collections::HashMap;

use askama::Template;
use axum::response::{Html, IntoResponse};
use nfl_data::PlayerIdentity;
use serde::Deserialize;

use crate::admin::salary_imports::model::{RowGroup, RowState, SalaryImport, SalaryImportRow};
use crate::admin::salary_imports::store as import_store;
use crate::chrome::Chrome;
use crate::{AppError, AppState};

/// The index/list chrome for the salary-imports subtree.
pub fn imports_chrome() -> Chrome {
    Chrome::focused("Salary imports", "/admin")
}

/// The chrome for one import, back-linking to the list.
pub fn import_chrome(id: i64) -> Chrome {
    Chrome::focused(format!("Import #{id}"), "/admin/salary-imports")
}

#[derive(Template)]
#[template(path = "admin/salary_imports/index.html")]
pub struct IndexTemplate {
    pub imports: Vec<SalaryImport>,
    pub error: Option<String>,
    pub offending: Vec<String>,
    pub chrome: Chrome,
}

impl IndexTemplate {
    pub fn new(imports: Vec<SalaryImport>) -> Self {
        Self {
            imports,
            error: None,
            offending: vec![],
            chrome: imports_chrome(),
        }
    }
}

pub struct ReviewRow {
    pub id: i64,
    pub fd_name: String,
    pub fd_team: String,
    pub dfs_position: String, // "QB" | "RB" | "WR" | "TE" | "D/ST"
    pub salary: i64,
    pub is_unmatched: bool,
    pub state_label: String, // "matched" | "D/ST" | "resolved" | "skipped" | ""
    /// The player this row was resolved to, e.g. "Josh Allen (QB)". Empty unless
    /// the row is resolved.
    pub resolved_label: String,
    /// A likely-but-unproven match the admin can take in one click. `None`
    /// unless the row is unmatched and the importer had an opinion.
    pub suggestion: Option<Suggestion>,
}

/// A one-click answer offered on an unmatched row.
pub struct Suggestion {
    pub gsis_id: String,
    /// e.g. "Bam Knight (RB)" — the admin has to see who this is to judge it.
    pub label: String,
}

impl ReviewRow {
    fn build(row: SalaryImportRow, resolved_label: String, suggestion: Option<Suggestion>) -> Self {
        let is_unmatched = row.state == RowState::Unmatched;
        let state_label = match row.state {
            RowState::Matched => "matched",
            RowState::Dst => "D/ST",
            RowState::Resolved => "resolved",
            RowState::Skipped => "skipped",
            RowState::Unmatched => "",
        }
        .to_string();
        Self {
            id: row.id,
            fd_name: row.fd_name,
            fd_team: row.fd_team,
            dfs_position: row.dfs_position.label().to_string(),
            salary: row.salary,
            is_unmatched,
            state_label,
            resolved_label,
            suggestion,
        }
    }
}

/// Everything the swappable `#triage` region needs. Held by `ReviewTemplate`,
/// which renders it inline on a full page load and standalone for the htmx swap
/// via `as_triage()`, from one `{% block triage %}` definition — so a decision
/// swap and a full page load cannot drift apart.
pub struct TriageView {
    pub import_id: i64,
    pub group_slug: &'static str,
    pub status: String,
    pub auto_matched: i64,
    pub matched: i64,
    pub dst: i64,
    pub unmatched: i64,
    pub tabs: Vec<GroupTab>,
    pub rows: Vec<ReviewRow>,
    pub empty_message: String,
}

/// The review page is a title wrapped around the triage region. Nothing a
/// decision can change lives outside it — see `TriageView`.
#[derive(Template)]
#[template(path = "admin/salary_imports/review.html", blocks = ["triage"])]
pub struct ReviewTemplate {
    pub triage: TriageView,
    pub chrome: Chrome,
}

/// One entry in the group selector.
pub struct GroupTab {
    pub slug: &'static str,
    pub label: &'static str,
    pub count: i64,
    pub selected: bool,
    /// Where the address bar should point once this tab is showing.
    pub url: String,
}

#[derive(Deserialize)]
pub struct GroupQuery {
    #[serde(default)]
    group: Option<String>,
}

impl GroupQuery {
    /// Unknown or absent values fall back to `Unmatched`; a bad query param
    /// must never fail the request.
    pub fn group(&self) -> RowGroup {
        self.group
            .as_deref()
            .and_then(RowGroup::from_slug)
            .unwrap_or_default()
    }
}

/// The label a resolved row wears: the player the admin picked, positioned when
/// known.
///
/// Two sources, because a pick can come from either. The team list is built
/// from weekly rosters, and 144 of 2025's 3,133 rostered players have no row in
/// the `players` table — and those are precisely the ones resolved by hand,
/// since `plan()` matches against `players`, so a player missing from it can
/// never auto-match. The widened search reads `players`. Checking only one
/// source reduces the other's picks to a bare gsis id.
///
/// A pick neither source knows still falls back to the raw id rather than an
/// empty arrow, so the record of what the admin chose survives.
fn label_for(gsis: &str, names: &HashMap<String, PlayerIdentity>) -> String {
    match names.get(gsis) {
        Some(PlayerIdentity {
            name,
            position: Some(pos),
            ..
        }) => format!("{name} ({pos})"),
        Some(PlayerIdentity { name, .. }) => name.clone(),
        None => gsis.to_string(),
    }
}

pub async fn build_triage(
    state: &AppState,
    import_id: i64,
    group: RowGroup,
) -> Result<Option<TriageView>, AppError> {
    let Some(batch) = import_store::get(state.db.reader(), import_id).await? else {
        return Ok(None);
    };
    let counts = import_store::state_counts(state.db.reader(), import_id).await?;
    let rows = import_store::decision_rows_for(state.db.reader(), import_id, group).await?;

    // Ids needing a name: the pick a resolved row carries, and the suggestion
    // an unmatched one offers. Skipped rows carry neither.
    let ids: Vec<&str> = rows
        .iter()
        .filter_map(|r| match group {
            RowGroup::Resolved => r.gsis_player_id.as_deref(),
            RowGroup::Unmatched => r.suggested_gsis_player_id.as_deref(),
            RowGroup::Skipped => None,
        })
        .collect();
    let names = state
        .nfl
        .identify(&ids)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let review_rows = rows
        .into_iter()
        .map(|row| {
            let resolved_label = match (group, row.gsis_player_id.as_deref()) {
                (RowGroup::Resolved, Some(gsis)) => label_for(gsis, &names),
                _ => String::new(),
            };
            let suggestion = match (group, row.suggested_gsis_player_id.as_deref()) {
                (RowGroup::Unmatched, Some(gsis)) => Some(Suggestion {
                    gsis_id: gsis.to_string(),
                    label: label_for(gsis, &names),
                }),
                _ => None,
            };
            ReviewRow::build(row, resolved_label, suggestion)
        })
        .collect();

    let tabs = RowGroup::ALL
        .into_iter()
        .map(|g| GroupTab {
            slug: g.slug(),
            label: g.label(),
            count: counts.count_for(g),
            selected: g == group,
            url: format!("/admin/salary-imports/{import_id}{}", g.path_suffix()),
        })
        .collect();

    Ok(Some(TriageView {
        import_id,
        group_slug: group.slug(),
        status: batch.status,
        auto_matched: counts.auto_matched(),
        matched: counts.matched,
        dst: counts.dst,
        unmatched: counts.unmatched,
        tabs,
        rows: review_rows,
        empty_message: group.empty_message().to_string(),
    }))
}

fn static_row(row: SalaryImportRow, detail: String) -> StaticRow {
    StaticRow {
        fd_name: row.fd_name,
        fd_team: row.fd_team,
        dfs_position: row.dfs_position.label().to_string(),
        salary: row.salary,
        detail,
    }
}

/// One row of a committed import: a record, so no controls.
pub struct StaticRow {
    pub fd_name: String,
    pub fd_team: String,
    pub dfs_position: String,
    pub salary: i64,
    /// e.g. "→ Josh Allen (QB)". Empty when there is nothing to add.
    pub detail: String,
}

#[derive(Template)]
#[template(path = "admin/salary_imports/committed.html")]
pub struct CommittedTemplate {
    pub week: Option<i64>,
    pub games: i64,
    pub committed_at: Option<String>,
    pub imported: i64,
    pub matched: i64,
    pub dst: i64,
    pub resolved: i64,
    pub skipped: i64,
    pub skipped_rows: Vec<StaticRow>,
    pub resolved_rows: Vec<StaticRow>,
    pub chrome: Chrome,
}

/// What a committed import shows instead of the triage screen: what landed, and
/// what a human deliberately left out. No decisions remain, so offering Commit,
/// Discard, Skip or Undo here would be offering to change a record.
async fn committed_view(
    state: &AppState,
    batch: SalaryImport,
) -> Result<axum::response::Response, AppError> {
    let import_id = batch.id;
    let counts = import_store::state_counts(state.db.reader(), import_id).await?;
    let skipped = import_store::decision_rows_for(state.db.reader(), import_id, RowGroup::Skipped)
        .await?
        .into_iter()
        .map(|r| static_row(r, String::new()))
        .collect();

    // A resolved row has to name who it landed on — that is the decision
    // being recorded.
    let resolved_rows =
        import_store::decision_rows_for(state.db.reader(), import_id, RowGroup::Resolved).await?;
    let ids: Vec<&str> = resolved_rows
        .iter()
        .filter_map(|r| r.gsis_player_id.as_deref())
        .collect();
    let names = state
        .nfl
        .identify(&ids)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let resolved = resolved_rows
        .into_iter()
        .map(|r| {
            let detail = match r.gsis_player_id.as_deref() {
                Some(gsis) => format!("→ {}", label_for(gsis, &names)),
                None => String::new(),
            };
            static_row(r, detail)
        })
        .collect();

    let view = CommittedTemplate {
        week: batch.week,
        games: batch.games,
        committed_at: batch.committed_at,
        imported: counts.matched + counts.dst + counts.resolved,
        matched: counts.matched,
        dst: counts.dst,
        resolved: counts.resolved,
        skipped: counts.skipped,
        skipped_rows: skipped,
        resolved_rows: resolved,
        chrome: import_chrome(import_id),
    };
    Ok(Html(view.render()?).into_response())
}

/// The whole review page, opened on one group. Each group is its own route —
/// see `RowGroup::path_suffix` — so a tab can be linked, bookmarked and
/// reloaded, and the htmx tab swap has a real URL to push.
pub async fn review_group(
    state: AppState,
    id: i64,
    group: RowGroup,
) -> Result<axum::response::Response, AppError> {
    let Some(batch) = import_store::get(state.db.reader(), id).await? else {
        return Err(AppError::NotFound);
    };
    // A committed batch has no decisions left to make; every triage control on
    // it would offer to change a record.
    if batch.status == "committed" {
        return committed_view(&state, batch).await;
    }
    let Some(triage) = build_triage(&state, id, group).await? else {
        return Err(AppError::NotFound);
    };
    let view = ReviewTemplate {
        triage,
        chrome: import_chrome(id),
    };
    Ok(Html(view.render()?).into_response())
}
