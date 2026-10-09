use crate::{
    NflData,
    salary_imports::{
        candidates::CandidateQuery,
        model::{RowGroup, SalaryImport, SalaryImportRow, StateCounts},
        store,
    },
    web::error::AdminError,
};
use askama::Template;

#[derive(Template)]
#[template(
    path = "admin/salary_imports/index.html",
    config = "src/web/askama.toml"
)]
pub(super) struct IndexTemplate {
    pub(super) imports: Vec<SalaryImport>,
    pub(super) error: Option<String>,
}

#[derive(Template)]
#[template(
    path = "admin/salary_imports/review.html",
    config = "src/web/askama.toml",
    blocks = ["panel"]
)]
pub(super) struct ReviewTemplate {
    pub(super) import: SalaryImport,
    pub(super) rows: Vec<SalaryImportRow>,
    pub(super) counts: StateCounts,
    pub(super) group: RowGroup,
    pub(super) error: Option<String>,
}

#[derive(Template)]
#[template(
    path = "admin/salary_imports/committed.html",
    config = "src/web/askama.toml"
)]
pub(super) struct CommittedTemplate {
    pub(super) import: SalaryImport,
    pub(super) rows: Vec<SalaryImportRow>,
    pub(super) counts: StateCounts,
}

#[derive(Template)]
#[template(
    source = "{% if result.too_short %}<p class=\"mt-2 text-sm\">Enter at least two characters to widen the player search.</p>{% endif %}<form class=\"mt-3\" hx-get=\"/nfl-data-admin/salary-imports/{{ import_id }}/rows/{{ row.id }}/players\" hx-target=\"#candidate-{{ row.id }}\"><label>Search {{ row.fd_team }} candidates <input name=\"q\" value=\"{{ query }}\" minlength=\"{% if result.widened %}2{% else %}1{% endif %}\"></label><button class=\"btn btn-sm\">Search</button><label><input type=\"checkbox\" name=\"widened\" value=\"1\"{% if result.widened %} checked{% endif %}> Search across all teams and weeks</label></form><p class=\"mt-2 text-xs\">Candidates {{ source }}.</p>{% for candidate in result.candidates %}<form class=\"mt-2 flex items-center gap-3\" method=\"post\" action=\"/nfl-data-admin/salary-imports/{{ import_id }}/rows/{{ row.id }}\" hx-post=\"/nfl-data-admin/salary-imports/{{ import_id }}/rows/{{ row.id }}\" hx-target=\"#salary-review-panel\" hx-swap=\"outerHTML\"><input type=\"hidden\" name=\"action\" value=\"resolve\"><input type=\"hidden\" name=\"player_id\" value=\"{{ candidate.gsis_id }}\"><span>{{ candidate.label() }} · {{ candidate.gsis_id }}</span><button class=\"btn btn-xs btn-primary\">Confirm match</button></form>{% else %}{% if !result.too_short %}<p class=\"mt-2 text-sm\">No candidates found.</p>{% endif %}{% endfor %}",
    ext = "html"
)]
pub(super) struct CandidateResults {
    pub(super) import_id: i64,
    pub(super) row: SalaryImportRow,
    pub(super) result: CandidateQuery,
    query: String,
    source: String,
}
impl CandidateResults {
    pub(super) fn new(
        import_id: i64,
        row: SalaryImportRow,
        result: CandidateQuery,
        query: String,
    ) -> Self {
        let source = result.source.label(&row.fd_team);
        Self {
            import_id,
            row,
            result,
            query,
            source,
        }
    }
}

pub(super) struct SalaryPage;
impl SalaryPage {
    pub(super) async fn index(nfl: &NflData) -> Result<IndexTemplate, AdminError> {
        Ok(IndexTemplate {
            imports: store::list_recent(nfl.managed.reader(), 20).await?,
            error: None,
        })
    }
    pub(super) async fn review(
        nfl: &NflData,
        id: i64,
        group: RowGroup,
        error: Option<String>,
    ) -> Result<ReviewTemplate, sqlx::Error> {
        let import = store::get(nfl.managed.reader(), id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        let rows = store::grouped_rows(nfl.managed.reader(), id, group).await?;
        let counts = store::counts(nfl.managed.reader(), id).await?;
        Ok(ReviewTemplate {
            import,
            rows,
            counts,
            group,
            error,
        })
    }
    pub(super) async fn committed(
        nfl: &NflData,
        id: i64,
    ) -> Result<CommittedTemplate, sqlx::Error> {
        let import = store::get(nfl.managed.reader(), id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        let rows = store::rows_for(nfl.managed.reader(), id).await?;
        let counts = store::counts(nfl.managed.reader(), id).await?;
        Ok(CommittedTemplate {
            import,
            rows,
            counts,
        })
    }
}
