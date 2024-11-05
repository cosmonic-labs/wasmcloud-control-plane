use async_nats::RequestErrorKind;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LatticeError {
    #[error("nats error: {0}")]
    Nats(RequestErrorKind),
    #[error("error: {0}")]
    ApiError(String),
}
