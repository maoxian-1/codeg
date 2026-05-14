use chrono::Utc;
use std::collections::{HashMap, HashSet};

use sea_orm::DatabaseConnection;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, EntityTrait, IntoActiveModel, QueryOrder, Set,
    TransactionTrait,
};

use crate::app_error::AppCommandError;
use crate::db::entities::remote_workspace_connection;
use crate::db::error::DbError;
use crate::models::RemoteWorkspaceConnectionInfo;

fn to_info(model: remote_workspace_connection::Model) -> RemoteWorkspaceConnectionInfo {
    RemoteWorkspaceConnectionInfo {
        id: model.id,
        name: model.name,
        base_url: model.base_url,
        token: model.token,
        sort_order: model.sort_order,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}

pub fn normalize_base_url(raw: &str) -> Result<String, AppCommandError> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    let parsed = reqwest::Url::parse(&trimmed).map_err(|e| {
        AppCommandError::invalid_input("Remote Workspace URL is invalid").with_detail(e.to_string())
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(trimmed),
        _ => Err(AppCommandError::invalid_input(
            "Remote Workspace URL must use http or https",
        )),
    }
}

fn validate_name(name: &str) -> Result<String, AppCommandError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppCommandError::invalid_input(
            "Remote connection name is required",
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_token(token: &str) -> Result<String, AppCommandError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(AppCommandError::invalid_input(
            "Remote connection token is required",
        ));
    }
    Ok(trimmed.to_string())
}

pub async fn list(
    conn: &DatabaseConnection,
) -> Result<Vec<RemoteWorkspaceConnectionInfo>, DbError> {
    let rows = remote_workspace_connection::Entity::find()
        .order_by_asc(remote_workspace_connection::Column::SortOrder)
        .order_by_asc(remote_workspace_connection::Column::Name)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(to_info).collect())
}

pub async fn get(
    conn: &DatabaseConnection,
    id: i32,
) -> Result<Option<RemoteWorkspaceConnectionInfo>, DbError> {
    let row = remote_workspace_connection::Entity::find_by_id(id)
        .one(conn)
        .await?;
    Ok(row.map(to_info))
}

pub async fn create(
    conn: &DatabaseConnection,
    name: &str,
    base_url: &str,
    token: &str,
) -> Result<RemoteWorkspaceConnectionInfo, AppCommandError> {
    let now = Utc::now();
    let max_order = remote_workspace_connection::Entity::find()
        .order_by_desc(remote_workspace_connection::Column::SortOrder)
        .one(conn)
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?
        .map(|m| m.sort_order)
        .unwrap_or(-1);
    let active = remote_workspace_connection::ActiveModel {
        id: NotSet,
        name: Set(validate_name(name)?),
        base_url: Set(normalize_base_url(base_url)?),
        token: Set(validate_token(token)?),
        sort_order: Set(max_order + 1),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let model = active
        .insert(conn)
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?;
    Ok(to_info(model))
}

pub async fn update(
    conn: &DatabaseConnection,
    id: i32,
    name: &str,
    base_url: &str,
    token: &str,
) -> Result<RemoteWorkspaceConnectionInfo, AppCommandError> {
    let row = remote_workspace_connection::Entity::find_by_id(id)
        .one(conn)
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?
        .ok_or_else(|| AppCommandError::not_found(format!("Remote connection {id} not found")))?;

    let mut active = row.into_active_model();
    active.name = Set(validate_name(name)?);
    active.base_url = Set(normalize_base_url(base_url)?);
    active.token = Set(validate_token(token)?);
    active.updated_at = Set(Utc::now());
    let model = active
        .update(conn)
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?;
    Ok(to_info(model))
}

pub async fn delete(conn: &DatabaseConnection, id: i32) -> Result<(), DbError> {
    remote_workspace_connection::Entity::delete_by_id(id)
        .exec(conn)
        .await?;
    Ok(())
}

pub async fn reorder(conn: &DatabaseConnection, ids: Vec<i32>) -> Result<(), AppCommandError> {
    if ids.is_empty() {
        return Ok(());
    }

    let unique_ids = ids.iter().copied().collect::<HashSet<_>>();
    if unique_ids.len() != ids.len() {
        return Err(AppCommandError::invalid_input(
            "Remote workspace order contains duplicate connections",
        ));
    }

    let rows = remote_workspace_connection::Entity::find()
        .all(conn)
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?;
    let existing_ids = rows.iter().map(|row| row.id).collect::<HashSet<_>>();
    if existing_ids != unique_ids {
        return Err(AppCommandError::invalid_input(
            "Remote workspace order must include every connection exactly once",
        ));
    }

    let now = Utc::now();
    let mut rows_by_id = rows
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let txn = conn
        .begin()
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?;
    for (idx, id) in ids.into_iter().enumerate() {
        let Some(row) = rows_by_id.remove(&id) else {
            return Err(AppCommandError::invalid_input(
                "Remote workspace order contains an unknown connection",
            ));
        };
        let mut active = row.into_active_model();
        active.sort_order = Set(idx as i32);
        active.updated_at = Set(now);
        active
            .update(&txn)
            .await
            .map_err(DbError::from)
            .map_err(AppCommandError::db)?;
    }
    txn.commit()
        .await
        .map_err(DbError::from)
        .map_err(AppCommandError::db)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::fresh_in_memory_db;

    #[test]
    fn normalize_base_url_trims_and_removes_trailing_slashes() {
        let actual = normalize_base_url("  http://127.0.0.1:3080///  ").unwrap();
        assert_eq!(actual, "http://127.0.0.1:3080");
    }

    #[test]
    fn normalize_base_url_rejects_non_http_schemes() {
        let err = normalize_base_url("file:///tmp/codeg").unwrap_err();
        assert!(err.message.contains("http"));
    }

    #[tokio::test]
    async fn create_list_update_delete_roundtrip() {
        let db = fresh_in_memory_db().await;
        let created = create(
            &db.conn,
            "Local 3080",
            "http://127.0.0.1:3080/",
            "secret-token",
        )
        .await
        .unwrap();
        assert_eq!(created.name, "Local 3080");
        assert_eq!(created.base_url, "http://127.0.0.1:3080");
        assert_eq!(created.token, "secret-token");
        assert_eq!(created.sort_order, 0);

        let listed = list(&db.conn).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);

        let updated = update(
            &db.conn,
            created.id,
            "Server A",
            "https://codeg.example.com/",
            "next-token",
        )
        .await
        .unwrap();
        assert_eq!(updated.name, "Server A");
        assert_eq!(updated.base_url, "https://codeg.example.com");

        delete(&db.conn, created.id).await.unwrap();
        assert!(list(&db.conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reorder_updates_list_order() {
        let db = fresh_in_memory_db().await;
        let first = create(&db.conn, "First", "http://127.0.0.1:3080", "token-a")
            .await
            .unwrap();
        let second = create(&db.conn, "Second", "http://127.0.0.1:3081", "token-b")
            .await
            .unwrap();
        let third = create(&db.conn, "Third", "http://127.0.0.1:3082", "token-c")
            .await
            .unwrap();

        reorder(&db.conn, vec![third.id, first.id, second.id])
            .await
            .unwrap();

        let listed = list(&db.conn).await.unwrap();
        assert_eq!(
            listed.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![third.id, first.id, second.id]
        );
        assert_eq!(
            listed
                .iter()
                .map(|item| item.sort_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[tokio::test]
    async fn reorder_rejects_partial_or_duplicate_ids() {
        let db = fresh_in_memory_db().await;
        let first = create(&db.conn, "First", "http://127.0.0.1:3080", "token-a")
            .await
            .unwrap();
        let second = create(&db.conn, "Second", "http://127.0.0.1:3081", "token-b")
            .await
            .unwrap();

        let duplicate = reorder(&db.conn, vec![first.id, first.id])
            .await
            .unwrap_err();
        assert!(matches!(
            duplicate.code,
            crate::app_error::AppErrorCode::InvalidInput
        ));

        let partial = reorder(&db.conn, vec![second.id]).await.unwrap_err();
        assert!(matches!(
            partial.code,
            crate::app_error::AppErrorCode::InvalidInput
        ));

        let listed = list(&db.conn).await.unwrap();
        assert_eq!(
            listed.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
    }
}
