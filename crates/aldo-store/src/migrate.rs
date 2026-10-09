use sqlx::SqlitePool;

use crate::error::StoreError;

/// Applies pending migrations.
pub async fn run(pool: &SqlitePool) -> Result<(), StoreError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
