#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("could not open the database")]
    Store(#[from] aldo_store::StoreError),

    #[error("could not bind {address}: the port may already be in use")]
    Bind { address: std::net::SocketAddr },

    #[error("query was blank")]
    BlankQuery,

    #[error("the catalog search could not be recorded: {0}")]
    Catalog(#[from] aldo_catalog::CatalogSearchError),

    #[error("unknown candidate {0}")]
    UnknownCandidate(i64),
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;

        let public_message = match &self {
            Self::BlankQuery => self.to_string(),
            Self::UnknownCandidate(_) => self.to_string(),
            Self::Store(_) | Self::Bind { .. } | Self::Catalog(_) => {
                tracing::error!(error = %self, "API request failed");
                "internal server error".to_owned()
            }
        };

        // A blank query or an unknown candidate is the caller's mistake;
        // everything else is a fault on this machine.
        let status = match &self {
            Self::BlankQuery | Self::UnknownCandidate(_) => StatusCode::BAD_REQUEST,
            Self::Store(_) | Self::Bind { .. } | Self::Catalog(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (status, public_message).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
