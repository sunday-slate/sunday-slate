mod backend;
mod extract;
mod handlers;
pub mod middleware;
pub mod password;
pub mod reset;
pub(crate) mod store;
mod user;

use crate::AppState;
use axum::Router;
use axum::routing::{get, post};
pub use backend::Credentials;
pub use extract::{AdminUser, Commissioner, CurrentUser, MaybeUser};
use handlers::{
    forgot_password, handle_forgot_password, handle_login, handle_logout, handle_reset_password,
    handle_setup, login, reset_password, setup,
};
pub use middleware::bootstrap;

pub fn router() -> Router<AppState> {
    let router = Router::new()
        .route("/login", get(login).post(handle_login))
        .route("/logout", post(handle_logout))
        .route(
            "/forgot-password",
            get(forgot_password).post(handle_forgot_password),
        )
        .route(
            "/reset-password",
            get(reset_password).post(handle_reset_password),
        )
        .route("/setup", get(setup).post(handle_setup));

    // Test-only backdoor: establish a session directly from a user id,
    // bypassing the login form. Compiled out of release binaries.
    #[cfg(test)]
    let router = router.route("/__test/login/{user_id}", post(handlers::handle_test_login));

    router
}
