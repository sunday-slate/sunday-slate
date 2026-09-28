use axum_login::{AuthUser, AuthnBackend};

use super::password;
use crate::User;
use crate::users::store as users;
use crate::{AppError, Db};

#[derive(Clone)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

impl AuthnBackend for Db {
    type User = User;
    type Credentials = Credentials;
    type Error = AppError;

    async fn authenticate(&self, creds: Credentials) -> Result<Option<User>, AppError> {
        let Some(mut user) = users::find_by_email(self.reader(), &creds.email).await? else {
            return Ok(None);
        };

        if !password::verify(&creds.password, &user.password_hash) {
            return Ok(None);
        }

        // Transparently upgrade the stored hash when its params are outdated.
        if password::is_obsolete(&user.password_hash) {
            let new_hash = password::hash(&creds.password);
            self.write_tx(async |conn| -> Result<(), sqlx::Error> {
                users::update_password_hash(&mut *conn, user.id, &new_hash).await
            })
            .await?;
            user.password_hash = new_hash;
        }

        Ok(Some(user))
    }

    async fn get_user(&self, user_id: &<User as AuthUser>::Id) -> Result<Option<User>, AppError> {
        Ok(users::find_by_id(self.reader(), *user_id).await?)
    }
}
