use codex_monitor_core::{
    FieldIdSet, SafeErrorCode, SourceExtraction, SourceFailure, SourceHealth, SourceIdentity,
    UtcTimestamp,
};

/// Checkpoint shape a source can persist without exposing source internals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointCapability {
    None,
    MonotonicFileOffsetV1,
}

/// Immutable, versioned registration data for one telemetry source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetrySourceDefinition {
    identity: SourceIdentity,
    supported_fields: FieldIdSet,
    checkpoint: CheckpointCapability,
}

impl TelemetrySourceDefinition {
    #[must_use]
    pub const fn new(
        identity: SourceIdentity,
        supported_fields: FieldIdSet,
        checkpoint: CheckpointCapability,
    ) -> Self {
        Self {
            identity,
            supported_fields,
            checkpoint,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn supported_fields(&self) -> &FieldIdSet {
        &self.supported_fields
    }

    #[must_use]
    pub const fn checkpoint(&self) -> CheckpointCapability {
        self.checkpoint
    }
}

/// Versioned source adapter contract consumed by the runtime.
pub trait TelemetrySource: Send + Sync {
    type Input;

    fn definition(&self) -> &TelemetrySourceDefinition;

    fn health(&self) -> &SourceHealth;

    fn extract(&self, input: Self::Input, observed_at: UtcTimestamp) -> SourceExtraction;

    /// Update health only after the receiver has durably committed the batch.
    fn mark_committed(&mut self, committed_at: UtcTimestamp);

    /// Record a closed diagnostic code without retaining a raw error chain.
    fn mark_failure(&mut self, code: SafeErrorCode, occurred_at: UtcTimestamp) {
        self.health_mut()
            .mark_degraded(SourceFailure::new(code, occurred_at));
    }

    #[doc(hidden)]
    fn health_mut(&mut self) -> &mut SourceHealth;
}
