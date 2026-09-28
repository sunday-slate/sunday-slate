use crate::auth::password;
use crate::invites::store as invites;
use crate::leagues::League;
use crate::tests::factories;
use crate::tests::factories::LeagueOptions;
use fake::Fake;
use fake::faker::internet::en::SafeEmail;
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct InviteOptions {
    pub email: Option<String>,
    pub league: Option<League>,
}

#[derive(Debug, Clone)]
pub struct GeneratedInvite {
    pub id: i64,
    pub email: String,
    secret: String,
}

impl GeneratedInvite {
    pub fn token(&self) -> String {
        format!("{}.{}", self.id, self.secret)
    }
}

pub async fn invite(pool: &SqlitePool, options: InviteOptions) -> GeneratedInvite {
    let league = match options.league {
        Some(l) => l,
        None => {
            factories::league(pool, LeagueOptions::default())
                .await
                .league
        }
    };

    let email = options.email.unwrap_or_else(|| SafeEmail().fake());

    let secret = Uuid::new_v4().to_string();
    let hash = password::hash(&secret);

    let id = invites::upsert(pool, league.id, &email, &hash)
        .await
        .expect("factory: upsert invite");

    GeneratedInvite { id, secret, email }
}
