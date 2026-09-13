//! Bounded offloading for synchronous filesystem, log-validation, and Git reads.
//!
//! Permits belong to the blocking work, not the awaiting request. Disconnecting
//! a client cannot free capacity while its read is still consuming resources.

use crate::ApiError;
use axum::http::StatusCode;
use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;

const MAX_READ_WORK: usize = 8;
static READ_WORK: OnceLock<ReadWork> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct ReadWork {
    permits: Arc<Semaphore>,
}

impl ReadWork {
    fn new(capacity: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(capacity)),
        }
    }

    pub(crate) async fn run<T, F>(&self, work: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: "repository readers are busy; retry shortly".into(),
                code: None,
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| ApiError::internal(format!("repository reader failed: {error}")))?
    }
}

impl Default for ReadWork {
    fn default() -> Self {
        Self::new(MAX_READ_WORK)
    }
}

pub(crate) async fn run<T, F>(work: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    READ_WORK
        .get_or_init(|| ReadWork::new(MAX_READ_WORK))
        .run(work)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn read_work_keeps_runtime_responsive_and_retains_capacity_after_cancellation() {
        let work = Arc::new(ReadWork::new(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task_work = work.clone();
        let task = tokio::spawn(async move {
            task_work
                .run(move || {
                    let _ = started_tx.send(());
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(())
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();
        // A synchronous read on this single runtime thread would deadlock here.
        let health = tokio::time::timeout(Duration::from_secs(1), crate::rest::health())
            .await
            .unwrap();
        assert_eq!(health.0["ok"], true);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let error = work.run(|| Ok(())).await.unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while work.permits.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(work.run(|| Ok(7)).await.unwrap(), 7);
    }

    #[tokio::test]
    async fn read_work_releases_capacity_after_failure_and_panic() {
        let work = ReadWork::new(1);
        assert_eq!(
            work.run::<(), _>(|| Err(ApiError::bad_request("bad log")))
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            work.run::<(), _>(|| panic!("fixture reader panic"))
                .await
                .unwrap_err()
                .status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(work.run(|| Ok(())).await.is_ok());
    }
}

#[cfg(test)]
#[path = "read_work_tests.rs"]
mod handler_tests;
