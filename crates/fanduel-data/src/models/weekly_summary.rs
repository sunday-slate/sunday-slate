use nfl_model::SeasonWeek;

pub struct WeeklySummary {
    pub season_week: SeasonWeek,
    pub salary_count: usize,
    pub unmatched_count: usize,
}
