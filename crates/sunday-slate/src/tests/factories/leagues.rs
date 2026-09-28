use crate::fantasy_teams::FantasyTeam;
use crate::leagues::League;
use crate::tests::factories;
use crate::tests::factories::TeamOptions;
use crate::{User, leagues};
use fake::Fake;
use fake::faker::company::en::CompanyName;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Default)]
pub struct LeagueOptions {
    pub name: Option<String>,
    pub commish: Option<User>,
}

#[derive(Debug, Clone)]
pub struct GeneratedLeague {
    pub league: League,
    pub commish: User,
    pub commish_team: FantasyTeam,
}

pub async fn league(pool: &SqlitePool, options: LeagueOptions) -> GeneratedLeague {
    let league = bare_league(pool, options.name).await;
    let gen_team = factories::team(
        pool,
        TeamOptions {
            league: Some(league),
            owner: options.commish,
            is_commish: true,
            ..Default::default()
        },
    )
    .await;

    GeneratedLeague {
        league: gen_team.league,
        commish: gen_team.owner,
        commish_team: gen_team.team,
    }
}

pub async fn bare_league(pool: &SqlitePool, name: Option<String>) -> League {
    // Apostrophe-bearing names (O'Keefe Inc) get HTML-escaped by askama and
    // break tests that assert the raw name appears in a page body.
    let name = name.unwrap_or_else(|| CompanyName().fake::<String>().replace('\'', ""));
    leagues::store::create(pool, &name)
        .await
        .expect("factory: create league")
}
