use super::is_htmx;
use crate::{AppError, AppState};
use askama::Template;
use axum::Form;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::response::{Html, IntoResponse, Response};
use garde::Report;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

/// Maps field names to ordered error messages, built from a `garde::Report`.
#[derive(Debug, Default, Clone)]
pub struct FormErrors(HashMap<String, Vec<String>>);

impl From<&Report> for FormErrors {
    fn from(value: &Report) -> Self {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (path, err) in value.iter() {
            let field = path.to_string();
            map.entry(field)
                .or_default()
                .push(err.message().to_string());
        }
        Self(map)
    }
}

impl FormErrors {
    /// Return the error messages for the named field, or an empty slice
    /// if the field has no errors.
    pub fn field(&self, name: &str) -> &[String] {
        self.0.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Build a one-field, one-message error set (for checks garde can't express,
    /// e.g. a database uniqueness rule).
    pub fn single(field: &str, message: &str) -> Self {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert(field.to_string(), vec![message.to_string()]);
        Self(map)
    }
}

pub trait FormView: Template {
    fn render_form(&self) -> Result<String, askama::Error>;
}

pub fn form_response(htmx: bool, view: &impl FormView) -> Result<Response, AppError> {
    let html = if htmx {
        view.render_form()?
    } else {
        view.render()?
    };
    Ok(Html(html).into_response())
}

#[macro_export]
macro_rules! form_view {
    ($t:ty) => {
        impl $crate::web::FormView for $t {
            fn render_form(&self) -> ::std::result::Result<String, ::askama::Error> {
                self.as_form().render()
            }
        }
    };
}

pub trait FormInput: DeserializeOwned + garde::Validate<Context = ()> + Send {
    type View: FormView;
    /// Request context the error view needs beyond the input itself, such
    /// as the route's team. `()` when the input alone fills the view.
    type Ctx: FromRequestParts<AppState> + Send;
    fn normalize(&mut self) {}
    fn to_view(&self, ctx: Self::Ctx, errors: FormErrors) -> Self::View; // owns the prefill (and which fields to omit)
}

pub struct Valid<T>(pub T);

impl<T: FormInput> FromRequest<AppState> for Valid<T> {
    type Rejection = Response;
    async fn from_request(req: Request, state: &AppState) -> Result<Self, Response> {
        let (mut parts, body) = req.into_parts();
        let htmx = is_htmx(&parts.headers);
        let ctx = T::Ctx::from_request_parts(&mut parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let Form(mut input) = Form::<T>::from_request(Request::from_parts(parts, body), state)
            .await
            .map_err(IntoResponse::into_response)?;
        input.normalize();
        if let Err(report) = input.validate() {
            let view = input.to_view(ctx, FormErrors::from(&report));
            return Err(form_response(htmx, &view).map_err(IntoResponse::into_response)?);
        }
        Ok(Valid(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::password::Password;
    use garde::Validate;

    #[test]
    fn form_errors_empty_is_empty() {
        let fe = FormErrors::default();
        assert!(fe.is_empty());
        assert!(fe.field("email").is_empty());
    }

    #[test]
    fn form_errors_from_on_valid_form() {
        #[derive(Validate)]
        struct F {
            #[garde(length(min = 1))]
            name: String,
        }
        let f = F {
            name: "a".to_string(),
        };
        let fe = match f.validate() {
            Ok(()) => FormErrors::default(),
            Err(report) => FormErrors::from(&report),
        };
        assert!(fe.is_empty());
    }

    #[test]
    fn form_errors_from_single_field() {
        #[derive(Validate)]
        struct F {
            #[garde(email)]
            email: String,
        }
        let f = F {
            email: "not-an-email".to_string(),
        };
        let Err(report) = f.validate() else {
            panic!("expected error")
        };
        let fe = FormErrors::from(&report);
        assert!(!fe.is_empty());
        let msgs = fe.field("email");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("not a valid email"));
    }

    #[test]
    fn form_errors_from_two_errors_same_field() {
        let p = Password("sh".to_string());
        let Err(report) = p.validate() else {
            panic!("expected error")
        };
        let fe = FormErrors::from(&report);
        assert!(!fe.is_empty());
    }

    #[test]
    fn form_errors_keys_are_flat_with_transparent() {
        #[derive(Validate)]
        struct F {
            #[garde(dive)]
            password: Password,
        }
        let f = F {
            password: Password("sh ort".to_string()),
        };
        let Err(report) = f.validate() else {
            panic!("expected error")
        };
        let fe = FormErrors::from(&report);
        assert!(fe.field("password.0").is_empty());
        assert!(!fe.field("password").is_empty());
        let msgs = fe.field("password");
        assert!(msgs.iter().any(|m| m.contains("length")));
        assert!(msgs.iter().any(|m| m.contains("spaces")));
    }

    #[test]
    fn form_errors_field_nonexistent_returns_empty() {
        let fe = FormErrors::default();
        assert!(fe.field("nonexistent").is_empty());
    }

    #[test]
    fn form_errors_single_holds_one_message() {
        let fe = FormErrors::single("email", "already a member");
        assert!(!fe.is_empty());
        assert_eq!(fe.field("email"), vec!["already a member"]);
    }
}
