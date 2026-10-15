use crate::admin::salary_imports::{self as import, SalaryWeekOverview};
use crate::chrome::Chrome;
use crate::{AppError, AppState};
use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use nfl_data::SeasonWeek;

struct SalaryRowView {
    player_id: u32,
    first_name: String,
    last_name: String,
    team: String,
    position: String,
    salary: u32,
}
struct WeekView {
    encoded: u32,
    season: u16,
    season_type: String,
    week: u8,
    salary_count: usize,
    unmatched: Vec<SalaryRowView>,
}
#[derive(Template)]
#[template(path = "admin/salary_imports/week.html")]
struct WeekTemplate {
    overview: WeekView,
    chrome: Chrome,
}
impl WeekTemplate {
    fn new(value: SalaryWeekOverview) -> Self {
        let sw = value.season_week;
        let season_type = if sw.season_type == nfl_data::SeasonType::Post {
            "POST"
        } else {
            "REG"
        }
        .to_owned();
        Self {
            overview: WeekView {
                encoded: sw.encode(),
                season: sw.season.0,
                season_type,
                week: sw.week.0,
                salary_count: value.salary_count,
                unmatched: value
                    .unmatched
                    .into_iter()
                    .filter_map(|s| {
                        s.player.map(|p| SalaryRowView {
                            player_id: p.id,
                            first_name: p.first_name,
                            last_name: p.last_name,
                            team: s.team.as_str().to_owned(),
                            position: String::from(s.position),
                            salary: s.salary,
                        })
                    })
                    .collect(),
            },
            chrome: Chrome::focused("Salary week", "/admin/salary-imports"),
        }
    }
}

pub async fn week(
    State(state): State<AppState>,
    Path(encoded): Path<u32>,
) -> Result<impl IntoResponse, AppError> {
    let season_week = SeasonWeek::decode(encoded).ok_or(AppError::NotFound)?;
    let overview = import::salary_week_overview(&state.db, &state.nfl, season_week)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Html(WeekTemplate::new(overview).render()?))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{dashboard_app, upload_form};
    use http::StatusCode;

    #[tokio::test]
    async fn week_lists_only_unmatched_players() {
        let app = dashboard_app().await;
        app.server
            .post("/admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = app.get("/admin/salary-imports/2025001").await;
        response.assert_status_ok();
        response.assert_text_contains("2025 · REG · week 1");
        response.assert_text_contains("3 source salaries · 1 unresolved");
        response.assert_text_contains("Mystery Receiver");
        response.assert_text_contains("/admin/salary-imports/2025001/players/999");
        assert!(!response.text().contains("Josh Allen"));
    }

    #[tokio::test]
    async fn week_rejects_invalid_encoded_week() {
        let app = dashboard_app().await;
        app.get("/admin/salary-imports/2025201")
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}
