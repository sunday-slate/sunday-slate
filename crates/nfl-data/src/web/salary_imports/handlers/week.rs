use crate::salaries::SalaryWeekOverview;
use crate::web::AdminState;
use crate::web::salary_imports::SalaryAdminError;
use askama::Template;
use axum::extract::{Path, State};
use axum::response::Html;

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
#[template(
    path = "admin/salary_imports/week.html",
    config = "src/web/askama.toml"
)]
struct WeekTemplate {
    overview: WeekView,
}
impl WeekTemplate {
    fn new(value: SalaryWeekOverview) -> Self {
        let sw = value.season_week;
        let season_type = if sw.season_type == crate::SeasonType::Post {
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
        }
    }
}

pub async fn week(
    State(state): State<AdminState>,
    Path(encoded): Path<u32>,
) -> Result<Html<String>, SalaryAdminError> {
    let season_week = crate::SeasonWeek::decode(encoded).ok_or(SalaryAdminError::NotFound)?;
    let overview = state.nfl.salary_week_overview(season_week).await?;
    Ok(Html(WeekTemplate::new(overview).render()?))
}

#[cfg(test)]
mod tests {
    use crate::web::tests::salary_fixture::{admin_state, upload_form};
    use axum::http::StatusCode;

    #[tokio::test]
    async fn week_lists_only_unmatched_players() {
        let state = admin_state().await;
        state
            .server
            .post("/nfl-data-admin/salary-imports")
            .multipart(upload_form())
            .await
            .assert_status(StatusCode::SEE_OTHER);

        let response = state
            .server
            .get("/nfl-data-admin/salary-imports/2025001")
            .await;
        response.assert_status_ok();
        response.assert_text_contains("2025 · REG · week 1");
        response.assert_text_contains("4 source salaries · 2 unresolved");
        response.assert_text_contains("Mystery Receiver");
        response.assert_text_contains("/nfl-data-admin/salary-imports/2025001/players/999");
        assert!(!response.text().contains("Josh Allen"));
    }

    #[tokio::test]
    async fn week_rejects_invalid_encoded_week() {
        let state = admin_state().await;
        state
            .server
            .get("/nfl-data-admin/salary-imports/2025201")
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}
