use std::{collections::HashMap, sync::Arc};

use askama::Template;
use axum::{
    Form, Router,
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_htmx::HxRequest;
use serde::Deserialize;

use crate::{
    NflData,
    salary_imports::{
        candidates,
        model::RowGroup,
        service::{ImportError, SalaryImportService},
        store,
    },
    web::error::AdminError,
};

use super::view::{CandidateResults, ResultsTemplate, SalaryPage};

pub(crate) fn router() -> Router<Arc<NflData>> {
    Router::new()
        .route("/salary-imports", get(index).post(upload))
        .route("/salary-imports/{id}", get(review))
        .route("/salary-imports/{id}/resolved", get(resolved))
        .route("/salary-imports/{id}/skipped", get(skipped))
        .route("/salary-imports/{id}/triage", get(triage))
        .route("/salary-imports/{id}/rows/{row_id}", post(decide))
        .route("/salary-imports/{id}/rows/{row_id}/undo", post(undo))
        .route("/salary-imports/{id}/rows/{row_id}/players", get(players))
        .route("/salary-imports/{id}/commit", post(commit))
        .route("/salary-imports/{id}/discard", post(discard))
}

#[derive(Debug, thiserror::Error)]
#[error("salary import request failed")]
struct SalaryHttpError(#[from] ImportError);

impl From<sqlx::Error> for SalaryHttpError {
    fn from(error: sqlx::Error) -> Self {
        if matches!(error, sqlx::Error::RowNotFound) {
            Self(ImportError::Missing)
        } else {
            Self(ImportError::Internal(error.to_string()))
        }
    }
}

impl IntoResponse for SalaryHttpError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            ImportError::Missing => StatusCode::NOT_FOUND,
            ImportError::Conflict => StatusCode::CONFLICT,
            ImportError::InvalidInput(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ImportError::Unavailable(_) | ImportError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %self.0, "NFL salary administration request failed");
            (status, "Internal server error").into_response()
        } else {
            (status, "Salary import request could not be completed").into_response()
        }
    }
}

async fn index(
    State(nfl): State<Arc<NflData>>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, AdminError> {
    let mut page = SalaryPage::index(&nfl).await?;
    page.error = query.get("error").cloned();
    Ok(page.render()?.into_response())
}

async fn upload(
    State(nfl): State<Arc<NflData>>,
    mut multipart: Multipart,
) -> Result<Response, Response> {
    let mut bytes = None;
    let mut filename = None;
    while let Some(field) = multipart.next_field().await.map_err(|error| {
        tracing::warn!(%error, "invalid salary upload multipart body");
        (StatusCode::UNPROCESSABLE_ENTITY, "Invalid multipart upload").into_response()
    })? {
        if field.name() == Some("file") {
            filename = field.file_name().map(str::to_owned);
            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, "failed reading salary upload");
                        (StatusCode::UNPROCESSABLE_ENTITY, "Invalid upload file").into_response()
                    })?
                    .to_vec(),
            );
        }
    }
    let Some(bytes) = bytes else {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "Choose a salary CSV file").into_response());
    };
    match SalaryImportService::new(&nfl)
        .upload(&bytes, filename.as_deref())
        .await
    {
        Ok(id) => Ok(Redirect::to(&format!("/nfl-data-admin/salary-imports/{id}")).into_response()),
        Err(ImportError::InvalidInput(_)) => {
            Ok(Redirect::to("/nfl-data-admin/salary-imports?error=invalid_upload").into_response())
        }
        Err(error) => Err(SalaryHttpError(error).into_response()),
    }
}

async fn review(
    State(nfl): State<Arc<NflData>>,
    Path(id): Path<i64>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, SalaryHttpError> {
    let import = store::get(nfl.managed.reader(), id)
        .await?
        .ok_or(ImportError::Missing)?;
    if import.status == "committed" {
        return SalaryPage::committed(&nfl, id)
            .await?
            .render()
            .map(|html| axum::response::Html(html).into_response())
            .map_err(|error| {
                tracing::error!(%error, "failed to render committed salary review");
                ImportError::Internal("render error".into()).into()
            });
    }
    let page =
        SalaryPage::review(&nfl, id, RowGroup::Unmatched, query.get("error").cloned()).await?;
    page.render()
        .map(|html| axum::response::Html(html).into_response())
        .map_err(|error| {
            tracing::error!(%error, "failed to render salary review");
            ImportError::Internal("render error".into()).into()
        })
}

async fn resolved(
    State(nfl): State<Arc<NflData>>,
    Path(id): Path<i64>,
    HxRequest(is_htmx): HxRequest,
) -> Result<Response, SalaryHttpError> {
    render_group(&nfl, id, RowGroup::Resolved, is_htmx).await
}
async fn skipped(
    State(nfl): State<Arc<NflData>>,
    Path(id): Path<i64>,
    HxRequest(is_htmx): HxRequest,
) -> Result<Response, SalaryHttpError> {
    render_group(&nfl, id, RowGroup::Skipped, is_htmx).await
}
async fn triage(
    State(nfl): State<Arc<NflData>>,
    Path(id): Path<i64>,
    HxRequest(is_htmx): HxRequest,
) -> Result<Response, SalaryHttpError> {
    render_group(&nfl, id, RowGroup::Unmatched, is_htmx).await
}

async fn render_group(
    nfl: &NflData,
    id: i64,
    group: RowGroup,
    is_htmx: bool,
) -> Result<Response, SalaryHttpError> {
    let page = SalaryPage::review(nfl, id, group, None).await?;
    if is_htmx {
        ResultsTemplate {
            import_id: id,
            rows: page.rows,
            group,
        }
        .render()
        .map(|html| axum::response::Html(html).into_response())
        .map_err(|error| {
            tracing::error!(%error, "failed to render salary review fragment");
            ImportError::Internal("render error".into()).into()
        })
    } else {
        page.render()
            .map(|html| axum::response::Html(html).into_response())
            .map_err(|error| {
                tracing::error!(%error, "failed to render salary review");
                ImportError::Internal("render error".into()).into()
            })
    }
}

#[derive(Deserialize)]
struct DecisionForm {
    action: String,
    #[serde(default)]
    player_id: String,
}

async fn decide(
    State(nfl): State<Arc<NflData>>,
    Path((id, row_id)): Path<(i64, i64)>,
    HxRequest(is_htmx): HxRequest,
    Form(form): Form<DecisionForm>,
) -> Result<Response, SalaryHttpError> {
    let service = SalaryImportService::new(&nfl);
    let result = match form.action.as_str() {
        "resolve" => service.resolve(id, row_id, &form.player_id).await,
        "skip" => service.skip(id, row_id).await,
        _ => return Err(ImportError::InvalidInput("unknown row action".into()).into()),
    };
    match result {
        Err(ImportError::InvalidInput(_)) => {
            return Ok(Redirect::to(&format!(
                "/nfl-data-admin/salary-imports/{id}?error=invalid_action"
            ))
            .into_response());
        }
        Err(error) => return Err(error.into()),
        Ok(()) => {}
    }
    if is_htmx {
        render_group(&nfl, id, RowGroup::Unmatched, true).await
    } else {
        Ok(Redirect::to(&format!("/nfl-data-admin/salary-imports/{id}")).into_response())
    }
}

async fn undo(
    State(nfl): State<Arc<NflData>>,
    Path((id, row_id)): Path<(i64, i64)>,
    HxRequest(is_htmx): HxRequest,
) -> Result<Response, SalaryHttpError> {
    SalaryImportService::new(&nfl).undo(id, row_id).await?;
    if is_htmx {
        render_group(&nfl, id, RowGroup::Unmatched, true).await
    } else {
        Ok(Redirect::to(&format!("/nfl-data-admin/salary-imports/{id}")).into_response())
    }
}

async fn players(
    State(nfl): State<Arc<NflData>>,
    Path((id, row_id)): Path<(i64, i64)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, SalaryHttpError> {
    let import = store::get(nfl.managed.reader(), id)
        .await?
        .ok_or(ImportError::Missing)?;
    if import.status != "pending" {
        return Err(ImportError::Conflict.into());
    }
    let row = store::rows_for(nfl.managed.reader(), id)
        .await?
        .into_iter()
        .find(|row| row.id == row_id)
        .ok_or(ImportError::Missing)?;
    let result = candidates::query(
        &nfl,
        crate::Season(import.season as u16),
        crate::Week(import.week as u8),
        &row.fd_team,
        query.get("q").map(String::as_str).unwrap_or(""),
        query.contains_key("widened"),
    )
    .await
    .map_err(|error| ImportError::Unavailable(error.to_string()))?;
    let html = CandidateResults::new(id, row, result, query.get("q").cloned().unwrap_or_default())
        .render()
        .map_err(|error| {
            tracing::error!(%error, "failed to render salary candidates");
            ImportError::Internal("render error".into())
        })?;
    Ok(axum::response::Html(html).into_response())
}

async fn commit(
    State(nfl): State<Arc<NflData>>,
    Path(id): Path<i64>,
) -> Result<Response, SalaryHttpError> {
    match SalaryImportService::new(&nfl).commit(id).await {
        Ok(_) => Ok(Redirect::to(&format!("/nfl-data-admin/salary-imports/{id}")).into_response()),
        Err(ImportError::InvalidInput(_)) => Ok(Redirect::to(&format!(
            "/nfl-data-admin/salary-imports/{id}?error=commit_invalid"
        ))
        .into_response()),
        Err(error) => Err(error.into()),
    }
}

async fn discard(
    State(nfl): State<Arc<NflData>>,
    Path(id): Path<i64>,
) -> Result<Response, SalaryHttpError> {
    SalaryImportService::new(&nfl).discard(id).await?;
    Ok(Redirect::to("/nfl-data-admin/salary-imports").into_response())
}
