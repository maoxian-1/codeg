use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RemoteWorkspaceConnection::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::Name)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::BaseUrl)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::Token)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::SortOrder)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RemoteWorkspaceConnection::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_remote_workspace_connection_sort_order")
                    .table(RemoteWorkspaceConnection::Table)
                    .col(RemoteWorkspaceConnection::SortOrder)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_remote_workspace_connection_base_url")
                    .table(RemoteWorkspaceConnection::Table)
                    .col(RemoteWorkspaceConnection::BaseUrl)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(RemoteWorkspaceConnection::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum RemoteWorkspaceConnection {
    Table,
    Id,
    Name,
    BaseUrl,
    Token,
    SortOrder,
    CreatedAt,
    UpdatedAt,
}
