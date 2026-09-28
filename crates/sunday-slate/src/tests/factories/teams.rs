use crate::User;
use crate::fantasy_teams::FantasyTeam;
use crate::fantasy_teams::store as fantasy_teams;
use crate::leagues::League;
use crate::tests::factories;
use crate::tests::factories::{LeagueOptions, UserOptions};
use fake::Fake;
use fake::faker::company::en::CompanyName;
use fake::faker::name::en::FirstName;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Default)]
pub struct TeamOptions {
    pub league: Option<League>,
    pub owner: Option<User>,
    pub name: Option<String>,
    pub owner_name: Option<String>,
    pub is_commish: bool,
}

#[derive(Debug, Clone)]
pub struct GeneratedTeam {
    pub league: League,
    pub owner: User,
    pub team: FantasyTeam,
}

pub async fn team(pool: &SqlitePool, options: TeamOptions) -> GeneratedTeam {
    let league = match options.league {
        Some(l) => l,
        None => {
            let gen_league = Box::pin(factories::league(pool, LeagueOptions::default())).await;
            gen_league.league
        }
    };
    let owner = match options.owner {
        Some(u) => u,
        None => {
            let gen_user = factories::user(pool, UserOptions::default()).await;
            gen_user.user
        }
    };
    // Apostrophe-bearing names (O'Conner) get HTML-escaped by askama and
    // break tests that assert the raw name appears in a page body.
    let name = options
        .name
        .unwrap_or_else(|| CompanyName().fake::<String>().replace('\'', ""));
    let owner_name = options.owner_name.unwrap_or_else(|| FirstName().fake());
    let is_commissioner = options.is_commish;

    let team = fantasy_teams::create(
        pool,
        league.id,
        owner.id,
        &name,
        &owner_name,
        is_commissioner,
    )
    .await
    .expect("factory: create team");

    GeneratedTeam {
        league,
        owner,
        team,
    }
}
