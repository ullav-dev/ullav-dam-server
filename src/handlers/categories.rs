use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::{
    AppState,
    error::{AppError, AppResult},
    models::category::{Category, CategoryWithChildren, CreateCategoryRequest, UpdateCategoryRequest},
};

// ── List all categories ───────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/categories",
    tag = "categories",
    responses(
        (status = 200, description = "List of all categories", body = Vec<Category>),
    )
)]
pub async fn list_categories(State(state): State<AppState>) -> AppResult<Json<Vec<Category>>> {
    let client = state.db.get().await?;
    let rows = client
        .query(
            "SELECT id, name, description, parent_id, created_at, updated_at
             FROM categories
             ORDER BY name ASC",
            &[],
        )
        .await?;

    let categories = rows.iter().map(Category::from).collect();
    Ok(Json(categories))
}

// ── Get one category with its direct sub-categories ──────────────────────────

#[utoipa::path(
    get,
    path = "/categories/{id}",
    tag = "categories",
    params(
        ("id" = uuid::Uuid, Path, description = "Category ID"),
    ),
    responses(
        (status = 200, description = "Category with its direct sub-categories", body = CategoryWithChildren),
        (status = 404, description = "Category not found", body = ErrorResponse),
    )
)]
pub async fn get_category(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<CategoryWithChildren>> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT id, name, description, parent_id, created_at, updated_at
             FROM categories WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Category {id} not found")))?;

    let category = Category::from(&row);

    let child_rows = client
        .query(
            "SELECT id, name, description, parent_id, created_at, updated_at
             FROM categories WHERE parent_id = $1 ORDER BY name ASC",
            &[&id],
        )
        .await?;

    let sub_categories = child_rows.iter().map(Category::from).collect();

    Ok(Json(CategoryWithChildren {
        category,
        sub_categories,
    }))
}

// ── Create category ───────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/categories",
    tag = "categories",
    request_body = CreateCategoryRequest,
    responses(
        (status = 201, description = "Category created", body = Category),
        (status = 400, description = "Invalid request or parent category not found", body = ErrorResponse),
    )
)]
pub async fn create_category(
    State(state): State<AppState>,
    Json(body): Json<CreateCategoryRequest>,
) -> AppResult<(StatusCode, Json<Category>)> {
    let client = state.db.get().await?;

    // Validate parent exists if provided
    if let Some(parent_id) = body.parent_id {
        let exists = client
            .query_opt("SELECT id FROM categories WHERE id = $1", &[&parent_id])
            .await?
            .is_some();
        if !exists {
            return Err(AppError::BadRequest(format!(
                "Parent category {parent_id} not found"
            )));
        }
    }

    let row = client
        .query_one(
            "INSERT INTO categories (name, description, parent_id)
             VALUES ($1, $2, $3)
             RETURNING id, name, description, parent_id, created_at, updated_at",
            &[&body.name, &body.description, &body.parent_id],
        )
        .await?;

    Ok((StatusCode::CREATED, Json(Category::from(&row))))
}

// ── Update category ───────────────────────────────────────────────────────────

#[utoipa::path(
    put,
    path = "/categories/{id}",
    tag = "categories",
    params(
        ("id" = uuid::Uuid, Path, description = "Category ID"),
    ),
    request_body = UpdateCategoryRequest,
    responses(
        (status = 200, description = "Updated category", body = Category),
        (status = 400, description = "Invalid update (e.g. self-referential parent)", body = ErrorResponse),
        (status = 404, description = "Category not found", body = ErrorResponse),
    )
)]
pub async fn update_category(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCategoryRequest>,
) -> AppResult<Json<Category>> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT id, name, description, parent_id, created_at, updated_at
             FROM categories WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Category {id} not found")))?;

    let current = Category::from(&row);

    let name = body.name.unwrap_or(current.name);
    let description = body.description.or(current.description);
    // `None` in the patch body is treated as "keep current"; use UpdateCategoryRequest
    // with an explicit sentinel if you need to clear parent_id.
    let parent_id = body.parent_id.or(current.parent_id);

    // Prevent self-referential cycle at direct parent level
    if let Some(pid) = parent_id {
        if pid == id {
            return Err(AppError::BadRequest(
                "A category cannot be its own parent".into(),
            ));
        }
    }

    let updated = client
        .query_one(
            "UPDATE categories SET name = $1, description = $2, parent_id = $3
             WHERE id = $4
             RETURNING id, name, description, parent_id, created_at, updated_at",
            &[&name, &description, &parent_id, &id],
        )
        .await?;

    Ok(Json(Category::from(&updated)))
}

// ── Delete category ───────────────────────────────────────────────────────────

#[utoipa::path(
    delete,
    path = "/categories/{id}",
    tag = "categories",
    params(
        ("id" = uuid::Uuid, Path, description = "Category ID"),
    ),
    responses(
        (status = 204, description = "Category deleted"),
        (status = 404, description = "Category not found", body = ErrorResponse),
    )
)]
pub async fn delete_category(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let client = state.db.get().await?;

    let result = client
        .execute("DELETE FROM categories WHERE id = $1", &[&id])
        .await?;

    if result == 0 {
        return Err(AppError::NotFound(format!("Category {id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
}
