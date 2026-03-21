#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("reth provider error: {0}")]
    Provider(#[from] reth_storage_errors::provider::ProviderError),

    #[error("reth database error: {0}")]
    Database(#[from] reth_storage_errors::db::DatabaseError),

    #[error("encoding error: {0}")]
    Encoding(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error(transparent)]
    Other(#[from] eyre::Report),
}

pub type Result<T> = std::result::Result<T, DbError>;
