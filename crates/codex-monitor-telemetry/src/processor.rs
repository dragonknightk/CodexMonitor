use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use codex_monitor_core::{SafeErrorCode, SourceFailure, SourceHealth, UtcTimestamp};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use tokio::sync::{Mutex, OwnedMutexGuard, oneshot, watch};
use tokio::task::{JoinError, JoinSet};

use crate::{
    CheckpointCapability, ObservationSink, ObservationSinkError, OtlpLogSource, OtlpProcessError,
    OtlpProcessOutcome, OtlpRequestProcessor, TelemetrySource,
};

/// commit-before-ack processor，拥有注册 OTLP source 的状态并通过窄 sink 持久化。
pub struct OtlpObservationProcessor {
    source: Arc<Mutex<OtlpLogSource>>,
    source_health: watch::Sender<SourceHealth>,
    checkpoint: CheckpointCapability,
    health_dirty: Arc<AtomicBool>,
    sink: Arc<dyn ObservationSink>,
    processing: Mutex<ProcessingSupervisor>,
}

impl OtlpObservationProcessor {
    #[must_use]
    pub fn new(source: OtlpLogSource, sink: Arc<dyn ObservationSink>) -> Self {
        let checkpoint = source.definition().checkpoint();
        let (source_health, _) = watch::channel(source.health().clone());
        Self {
            source: Arc::new(Mutex::new(source)),
            source_health,
            checkpoint,
            health_dirty: Arc::new(AtomicBool::new(false)),
            sink,
            processing: Mutex::new(ProcessingSupervisor::new()),
        }
    }

    pub async fn source_health(&self) -> SourceHealth {
        self.source.lock().await.health().clone()
    }

    #[must_use]
    pub fn monitor(&self) -> OtlpSourceHealthMonitor {
        OtlpSourceHealthMonitor {
            receiver: self.source_health.subscribe(),
            checkpoint: self.checkpoint,
        }
    }

    pub(crate) async fn mark_receiver_available(
        &self,
        occurred_at: UtcTimestamp,
    ) -> Result<(), ObservationSinkError> {
        self.update_health(HealthUpdate::Available, occurred_at)
            .await
    }

    pub(crate) async fn mark_receiver_unavailable(
        &self,
        code: SafeErrorCode,
        occurred_at: UtcTimestamp,
    ) -> Result<(), ObservationSinkError> {
        self.update_health(HealthUpdate::Unavailable(code), occurred_at)
            .await
    }

    pub(crate) async fn mark_receiver_stopped(
        &self,
        occurred_at: UtcTimestamp,
    ) -> Result<(), ObservationSinkError> {
        self.update_health(HealthUpdate::Stopped, occurred_at).await
    }

    async fn mark_processing_failure(
        &self,
        code: SafeErrorCode,
        occurred_at: UtcTimestamp,
    ) -> Result<(), ObservationSinkError> {
        self.update_health(HealthUpdate::Degraded(code), occurred_at)
            .await
    }

    async fn update_health(
        &self,
        update: HealthUpdate,
        occurred_at: UtcTimestamp,
    ) -> Result<(), ObservationSinkError> {
        let mut source = self.source.lock().await;
        match update {
            HealthUpdate::Available => source.health_mut().mark_available(occurred_at),
            HealthUpdate::Degraded(code) => source
                .health_mut()
                .mark_degraded(SourceFailure::new(code, occurred_at)),
            HealthUpdate::Unavailable(code) => source
                .health_mut()
                .mark_unavailable(SourceFailure::new(code, occurred_at)),
            HealthUpdate::Stopped => source.health_mut().mark_stopped(occurred_at),
        }
        self.source_health.send_replace(source.health().clone());
        self.health_dirty.store(true, Ordering::Release);
        let result = self.sink.store_health(source.health().clone()).await;
        if result.is_ok() {
            self.health_dirty.store(false, Ordering::Release);
        }
        result
    }

    pub(crate) async fn persist_source_health_if_dirty(&self) -> Result<(), ObservationSinkError> {
        if !self.health_dirty.load(Ordering::Acquire) {
            return Ok(());
        }
        let source = self.source.lock().await;
        if !self.health_dirty.load(Ordering::Acquire) {
            return Ok(());
        }
        self.sink.store_health(source.health().clone()).await?;
        self.health_dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn close_intake(&self) {
        let mut processing = self.processing.lock().await;
        processing.accepting = false;
        processing.reap_completed();
    }

    pub(crate) async fn drain_processing(&self) -> Result<(), JoinError> {
        let mut processing = self.processing.lock().await;
        processing.accepting = false;
        processing.drain().await
    }

    #[cfg(test)]
    pub(crate) async fn processing_count(&self) -> usize {
        let mut processing = self.processing.lock().await;
        processing.reap_completed();
        processing.tasks.len()
    }
}

#[derive(Clone)]
pub struct OtlpSourceHealthMonitor {
    receiver: watch::Receiver<SourceHealth>,
    checkpoint: CheckpointCapability,
}

impl OtlpSourceHealthMonitor {
    #[must_use]
    pub fn snapshot(&self) -> SourceHealth {
        self.receiver.borrow().clone()
    }

    #[must_use]
    pub const fn checkpoint(&self) -> CheckpointCapability {
        self.checkpoint
    }
}

struct ProcessingSupervisor {
    accepting: bool,
    tasks: JoinSet<()>,
    first_task_error: Option<JoinError>,
}

impl ProcessingSupervisor {
    fn new() -> Self {
        Self {
            accepting: true,
            tasks: JoinSet::new(),
            first_task_error: None,
        }
    }

    fn reap_completed(&mut self) {
        while let Some(result) = self.tasks.try_join_next() {
            if let Err(error) = result
                && self.first_task_error.is_none()
            {
                self.first_task_error = Some(error);
                self.accepting = false;
            }
        }
    }

    async fn drain(&mut self) -> Result<(), JoinError> {
        self.reap_completed();
        while let Some(result) = self.tasks.join_next().await {
            if let Err(error) = result
                && self.first_task_error.is_none()
            {
                self.first_task_error = Some(error);
            }
        }
        match self.first_task_error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Copy)]
enum HealthUpdate {
    Available,
    Degraded(SafeErrorCode),
    Unavailable(SafeErrorCode),
    Stopped,
}

async fn commit_request(
    mut source: OwnedMutexGuard<OtlpLogSource>,
    source_health: watch::Sender<SourceHealth>,
    health_dirty: Arc<AtomicBool>,
    sink: Arc<dyn ObservationSink>,
    request: ExportLogsServiceRequest,
    received_at: UtcTimestamp,
) -> Result<OtlpProcessOutcome, OtlpProcessError> {
    let extraction = source.extract(request, received_at);
    let rejected_log_records = extraction.rejected_records;

    let mut accepted_source = source.clone();
    accepted_source.mark_committed(received_at);
    match sink
        .commit(extraction, accepted_source.health().clone())
        .await
    {
        Ok(()) => {
            source.mark_committed(received_at);
            source_health.send_replace(source.health().clone());
            health_dirty.store(false, Ordering::Release);
            Ok(OtlpProcessOutcome::new(rejected_log_records))
        }
        Err(error) => {
            source.mark_failure(SafeErrorCode::PersistenceCommitFailed, received_at);
            source_health.send_replace(source.health().clone());
            health_dirty.store(true, Ordering::Release);
            if sink.store_health(source.health().clone()).await.is_err() {
                return Err(OtlpProcessError::retryable(
                    SafeErrorCode::PersistenceCommitFailed,
                ));
            }
            health_dirty.store(false, Ordering::Release);
            Err(map_sink_error(error))
        }
    }
}

impl OtlpRequestProcessor for OtlpObservationProcessor {
    fn process<'a>(
        &'a self,
        request: ExportLogsServiceRequest,
        received_at: UtcTimestamp,
    ) -> Pin<Box<dyn Future<Output = Result<OtlpProcessOutcome, OtlpProcessError>> + Send + 'a>>
    {
        let source = Arc::clone(&self.source);
        let source_health = self.source_health.clone();
        let health_dirty = Arc::clone(&self.health_dirty);
        let sink = Arc::clone(&self.sink);
        Box::pin(async move {
            let source = source.lock_owned().await;
            let (reply, response) = oneshot::channel();
            let mut processing = self.processing.lock().await;
            processing.reap_completed();
            if !processing.accepting || processing.first_task_error.is_some() {
                return Err(OtlpProcessError::retryable(
                    SafeErrorCode::SourceUnavailable,
                ));
            }
            processing.tasks.spawn(async move {
                let result = commit_request(
                    source,
                    source_health,
                    health_dirty,
                    sink,
                    request,
                    received_at,
                )
                .await;
                let _ = reply.send(result);
            });
            drop(processing);

            response.await.unwrap_or_else(|_| {
                Err(OtlpProcessError::retryable(
                    SafeErrorCode::PersistenceCommitFailed,
                ))
            })
        })
    }

    fn report_failure<'a>(
        &'a self,
        code: SafeErrorCode,
        occurred_at: UtcTimestamp,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let _ = self.mark_processing_failure(code, occurred_at).await;
        })
    }
}

fn map_sink_error(error: ObservationSinkError) -> OtlpProcessError {
    if error.is_retryable() {
        OtlpProcessError::retryable(SafeErrorCode::PersistenceCommitFailed)
    } else {
        OtlpProcessError::permanent(SafeErrorCode::PersistenceCommitFailed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use codex_monitor_core::{SourceExtraction, SourceHealth};
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;

    use super::*;
    use crate::{ObservationSinkErrorCode, OtlpLogSource};

    const FIXTURE: &str = include_str!("../../../fixtures/otlp/response_completed.json");

    #[derive(Default)]
    struct FakeSink {
        commits: StdMutex<Vec<(SourceExtraction, SourceHealth)>>,
        health: StdMutex<Vec<SourceHealth>>,
        commit_error: StdMutex<Option<ObservationSinkError>>,
        health_error: StdMutex<Option<ObservationSinkError>>,
    }

    impl ObservationSink for FakeSink {
        fn commit<'a>(
            &'a self,
            extraction: SourceExtraction,
            accepted_health: SourceHealth,
        ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>> {
            Box::pin(async move {
                if let Some(error) = *self.commit_error.lock().unwrap() {
                    return Err(error);
                }
                self.commits
                    .lock()
                    .unwrap()
                    .push((extraction, accepted_health));
                Ok(())
            })
        }

        fn store_health<'a>(
            &'a self,
            health: SourceHealth,
        ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>> {
            Box::pin(async move {
                if let Some(error) = *self.health_error.lock().unwrap() {
                    return Err(error);
                }
                self.health.lock().unwrap().push(health);
                Ok(())
            })
        }
    }

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn request() -> ExportLogsServiceRequest {
        serde_json::from_str(FIXTURE).unwrap()
    }

    #[tokio::test]
    async fn successful_ack_follows_one_typed_sink_commit() {
        let sink = Arc::new(FakeSink::default());
        let processor = OtlpObservationProcessor::new(
            OtlpLogSource::new(at(1)).unwrap(),
            Arc::clone(&sink) as Arc<dyn ObservationSink>,
        );

        let outcome = processor.process(request(), at(2)).await.unwrap();

        assert_eq!(outcome.rejected_log_records(), 0);
        {
            let commits = sink.commits.lock().unwrap();
            assert_eq!(commits.len(), 1);
            assert_eq!(commits[0].0.observations.len(), 1);
            assert_eq!(commits[0].1.last_accepted_at(), Some(at(2)));
        }
        assert_eq!(
            processor.source_health().await.last_accepted_at(),
            Some(at(2))
        );
    }

    #[tokio::test]
    async fn stable_sink_error_controls_retryability_without_leaking_details() {
        for (code, retryable) in [
            (ObservationSinkErrorCode::DomainRejected, false),
            (ObservationSinkErrorCode::Unavailable, true),
        ] {
            let sink = Arc::new(FakeSink::default());
            *sink.commit_error.lock().unwrap() = Some(ObservationSinkError::new(code));
            let processor = OtlpObservationProcessor::new(
                OtlpLogSource::new(at(1)).unwrap(),
                Arc::clone(&sink) as Arc<dyn ObservationSink>,
            );

            let error = processor.process(request(), at(2)).await.unwrap_err();
            assert_eq!(error.is_retryable(), retryable);
            assert_eq!(error.code(), SafeErrorCode::PersistenceCommitFailed);
        }
    }

    #[tokio::test]
    async fn dirty_health_is_retried_after_sink_recovers() {
        let sink = Arc::new(FakeSink::default());
        *sink.health_error.lock().unwrap() = Some(ObservationSinkError::new(
            ObservationSinkErrorCode::Unavailable,
        ));
        let processor = OtlpObservationProcessor::new(
            OtlpLogSource::new(at(1)).unwrap(),
            Arc::clone(&sink) as Arc<dyn ObservationSink>,
        );

        assert!(
            processor
                .mark_receiver_unavailable(SafeErrorCode::OtlpBindFailed, at(2))
                .await
                .is_err()
        );
        *sink.health_error.lock().unwrap() = None;
        processor.persist_source_health_if_dirty().await.unwrap();

        let health = sink.health.lock().unwrap();
        assert_eq!(health.len(), 1);
        assert_eq!(
            health[0].last_failure().unwrap().code(),
            SafeErrorCode::OtlpBindFailed,
        );
    }

    #[tokio::test]
    async fn sealed_intake_rejects_new_requests() {
        let sink = Arc::new(FakeSink::default());
        let processor = OtlpObservationProcessor::new(
            OtlpLogSource::new(at(1)).unwrap(),
            sink as Arc<dyn ObservationSink>,
        );
        processor.close_intake().await;

        let error = processor.process(request(), at(2)).await.unwrap_err();
        assert!(error.is_retryable());
        assert_eq!(error.code(), SafeErrorCode::SourceUnavailable);
    }
}
