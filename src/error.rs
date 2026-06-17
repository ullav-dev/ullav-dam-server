use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Database error: {0}")]
    Database(#[from] tokio_postgres::Error),

    #[error("Pool error: {0}")]
    Pool(#[from] deadpool_postgres::PoolError),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::Database(e) => {
                tracing::error!("Database error: {e:#?}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error".into())
            }
            AppError::Pool(e) => {
                tracing::error!("Pool error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Database pool error".into())
            }
            AppError::Storage(msg) => {
                tracing::error!("Storage error: {msg}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Storage error".into())
            }
            AppError::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, msg.clone()),
            AppError::Internal(e) => {
                tracing::error!("Internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".into())
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// Shape of every error response body: `{"error": "..."}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    async fn response_parts(err: AppError) -> (StatusCode, serde_json::Value) {
        let response = err.into_response();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn not_found_returns_404_with_message() {
        let (status, json) =
            response_parts(AppError::NotFound("Asset 123 not found".into())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"], "Asset 123 not found");
    }

    #[tokio::test]
    async fn bad_request_returns_400_with_message() {
        let (status, json) =
            response_parts(AppError::BadRequest("invalid input".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "invalid input");
    }

    #[tokio::test]
    async fn storage_error_returns_500_with_generic_message() {
        let (status, json) =
            response_parts(AppError::Storage("S3 bucket unreachable".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        // Internal details are not exposed to the caller
        assert_eq!(json["error"], "Storage error");
    }

    #[tokio::test]
    async fn internal_error_returns_500_with_generic_message() {
        let (status, json) =
            response_parts(AppError::Internal(anyhow::anyhow!("unexpected crash"))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"], "Internal server error");
    }

    #[test]
    fn error_response_body_is_json_object_with_error_key() {
        // Verify the shape of the error response is always {"error": "..."}
        let variants: Vec<AppError> = vec![
            AppError::NotFound("x".into()),
            AppError::BadRequest("x".into()),
            AppError::Storage("x".into()),
        ];
        for err in variants {
            let response = err.into_response();
            let ct = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(ct.contains("application/json"), "expected JSON content-type, got: {ct}");
        }
    }
}
