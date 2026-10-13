use crate::admin::salary_imports::SalaryWeekOverview;
use crate::chrome::Chrome;
use askama::Template;

#[derive(Template)]
#[template(path = "admin/salary_imports/index.html")]
pub struct DashboardTemplate {
    pub weeks: Vec<(u32, u16, String, u8, i64, usize)>,
    pub default_season: u16,
    pub default_week: u8,
    pub default_post: bool,
    pub chrome: Chrome,
}

pub struct SalaryRowView {
    pub player_id: String,
    pub first_name: String,
    pub last_name: String,
    pub team: String,
    pub position: String,
    pub salary: u32,
}
pub struct WeekView {
    pub encoded: u32,
    pub season: u16,
    pub season_type: String,
    pub week: u8,
    pub salary_count: usize,
    pub unmatched: Vec<SalaryRowView>,
}
#[derive(Template)]
#[template(path = "admin/salary_imports/week.html")]
pub struct WeekTemplate {
    pub overview: WeekView,
    pub chrome: Chrome,
}
impl WeekTemplate {
    pub fn new(value: SalaryWeekOverview) -> Self {
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
                            player_id: p.id.to_string(),
                            first_name: p.first_name,
                            last_name: p.last_name,
                            team: s.team.as_str().to_owned(),
                            position: s.position.label().to_owned(),
                            salary: s.salary,
                        })
                    })
                    .collect(),
            },
            chrome: Chrome::focused("Salary week", "/admin/salary-imports"),
        }
    }
}

#[derive(Template)]
#[template(path = "admin/salary_imports/resolve.html")]
pub struct ResolutionTemplate {
    pub week: u32,
    pub fd_player_id: String,
    pub results: super::candidates::CandidateResults,
    pub query: String,
    pub chrome: Chrome,
}
