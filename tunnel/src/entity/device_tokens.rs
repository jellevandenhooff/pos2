use chrono::Utc;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "device_tokens")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub device_code: String,
    #[sea_orm(column_type = "Text", unique)]
    pub user_code: String,
    pub state: TokenState,
    #[sea_orm(column_type = "Text")]
    pub approved_by_user_id: String,
    pub next_poll_at: chrono::DateTime<Utc>,
    pub interval_seconds: i64,
    pub expires_at: chrono::DateTime<Utc>,
}

#[derive(Clone, Eq, PartialEq, Debug, EnumIter, DeriveActiveEnum)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::None)",
    rename_all = "PascalCase"
)]
pub enum TokenState {
    Pending,
    Approved,
    Rejected,
    Exchanged,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
