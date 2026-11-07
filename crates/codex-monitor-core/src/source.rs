use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    FieldId, SafeErrorCode, SourceContractVersion, SourceId, UtcTimestamp, ValidationError,
};

/// Maximum distinct catalog fields one source contract may advertise.
pub const MAX_SOURCE_FIELDS: usize = 128;

/// Bounded, deterministic set of telemetry field capabilities.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FieldIdSet(BTreeSet<FieldId>);

impl FieldIdSet {
    /// Build a bounded set from already validated field IDs.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TooManySourceFields`] when the distinct field
    /// count exceeds [`MAX_SOURCE_FIELDS`]. Catalog membership is checked by the
    /// source registry before this set reaches persistence.
    pub fn new(fields: impl IntoIterator<Item = FieldId>) -> Result<Self, ValidationError> {
        let fields = fields.into_iter().collect::<BTreeSet<_>>();
        if fields.len() > MAX_SOURCE_FIELDS {
            return Err(ValidationError::TooManySourceFields {
                max: MAX_SOURCE_FIELDS,
                actual: fields.len(),
            });
        }
        Ok(Self(fields))
    }

    /// Return whether the source advertises one catalog field.
    #[must_use]
    pub fn contains(&self, field: &FieldId) -> bool {
        self.0.contains(field)
    }

    /// Iterate in stable field-ID order.
    pub fn iter(&self) -> impl Iterator<Item = &FieldId> {
        self.0.iter()
    }

    /// Return the number of distinct field capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Return whether no capabilities were declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Registered identity of one versioned telemetry source.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceIdentity {
    id: SourceId,
    contract_version: SourceContractVersion,
}

impl SourceIdentity {
    /// Create a registered source identity.
    #[must_use]
    pub const fn new(id: SourceId, contract_version: SourceContractVersion) -> Self {
        Self {
            id,
            contract_version,
        }
    }

    /// Return the stable source ID.
    #[must_use]
    pub const fn id(&self) -> &SourceId {
        &self.id
    }

    /// Return the source contract version.
    #[must_use]
    pub const fn contract_version(&self) -> SourceContractVersion {
        self.contract_version
    }
}

/// Observable lifecycle status of a telemetry source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealthStatus {
    Starting,
    Healthy,
    Degraded,
    Unavailable,
    Stopped,
}

/// Last privacy-safe failure recorded for a telemetry source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceFailure {
    code: SafeErrorCode,
    occurred_at: UtcTimestamp,
}

impl SourceFailure {
    /// Create a safe source failure without retaining an error chain or payload.
    #[must_use]
    pub const fn new(code: SafeErrorCode, occurred_at: UtcTimestamp) -> Self {
        Self { code, occurred_at }
    }

    /// Return the stable safe error code.
    #[must_use]
    pub const fn code(&self) -> SafeErrorCode {
        self.code
    }

    /// Return when the failure occurred.
    #[must_use]
    pub const fn occurred_at(&self) -> UtcTimestamp {
        self.occurred_at
    }
}

/// Current health, capabilities and safe diagnostics for a telemetry source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceHealth {
    identity: SourceIdentity,
    status: SourceHealthStatus,
    supported_fields: FieldIdSet,
    last_accepted_at: Option<UtcTimestamp>,
    last_failure: Option<SourceFailure>,
    updated_at: UtcTimestamp,
}

impl SourceHealth {
    /// Create source health in the starting state.
    ///
    /// # Errors
    ///
    /// Returns an error when the distinct capability set exceeds the domain
    /// bound.
    pub fn new(
        identity: SourceIdentity,
        supported_fields: impl IntoIterator<Item = FieldId>,
        now: UtcTimestamp,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            identity,
            status: SourceHealthStatus::Starting,
            supported_fields: FieldIdSet::new(supported_fields)?,
            last_accepted_at: None,
            last_failure: None,
            updated_at: now,
        })
    }

    /// Record an accepted source event and mark the source healthy.
    pub fn mark_accepted(&mut self, accepted_at: UtcTimestamp) {
        self.last_accepted_at = Some(
            self.last_accepted_at
                .map_or(accepted_at, |current| current.max(accepted_at)),
        );
        self.transition(SourceHealthStatus::Healthy, accepted_at);
    }

    /// Record that the source runtime is available without inventing an
    /// accepted telemetry timestamp.
    pub fn mark_available(&mut self, available_at: UtcTimestamp) {
        self.transition(SourceHealthStatus::Healthy, available_at);
    }

    /// Record a privacy-safe processing failure and mark the source degraded.
    pub fn mark_degraded(&mut self, failure: SourceFailure) {
        self.record_failure(SourceHealthStatus::Degraded, failure);
    }

    /// Record that the source capability is currently unavailable.
    pub fn mark_unavailable(&mut self, failure: SourceFailure) {
        self.record_failure(SourceHealthStatus::Unavailable, failure);
    }

    /// Mark the source stopped at the supplied instant.
    pub fn mark_stopped(&mut self, stopped_at: UtcTimestamp) {
        self.transition(SourceHealthStatus::Stopped, stopped_at);
    }

    fn record_failure(&mut self, status: SourceHealthStatus, failure: SourceFailure) {
        let occurred_at = failure.occurred_at();
        let replace = self.last_failure.as_ref().is_none_or(|current| {
            (occurred_at, failure.code()) >= (current.occurred_at(), current.code())
        });
        if replace {
            self.last_failure = Some(failure);
        }
        self.transition(status, occurred_at);
    }

    fn transition(&mut self, status: SourceHealthStatus, occurred_at: UtcTimestamp) {
        if (occurred_at, status) >= (self.updated_at, self.status) {
            self.status = status;
            self.updated_at = occurred_at;
        }
    }

    /// Return the registered source identity.
    #[must_use]
    pub const fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    /// Return the current health status.
    #[must_use]
    pub const fn status(&self) -> SourceHealthStatus {
        self.status
    }

    /// Return the source's catalog field capabilities.
    #[must_use]
    pub const fn supported_fields(&self) -> &FieldIdSet {
        &self.supported_fields
    }

    /// Return the most recent accepted-event time.
    #[must_use]
    pub const fn last_accepted_at(&self) -> Option<UtcTimestamp> {
        self.last_accepted_at
    }

    /// Return the most recent privacy-safe failure.
    #[must_use]
    pub const fn last_failure(&self) -> Option<&SourceFailure> {
        self.last_failure.as_ref()
    }

    /// Return the last health update time.
    #[must_use]
    pub const fn updated_at(&self) -> UtcTimestamp {
        self.updated_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            SourceId::new("synthetic-source").unwrap(),
            SourceContractVersion::new(1).unwrap(),
        )
    }

    #[test]
    fn source_health_keeps_monotonic_acceptance_and_safe_failure() {
        let mut health = SourceHealth::new(
            source(),
            [FieldId::new("model_call.model").unwrap()],
            at(10),
        )
        .unwrap();
        health.mark_accepted(at(20));
        health.mark_accepted(at(15));
        health.mark_degraded(SourceFailure::new(
            SafeErrorCode::OtlpJsonDecodeFailed,
            at(25),
        ));

        assert_eq!(health.last_accepted_at(), Some(at(20)));
        assert_eq!(health.status(), SourceHealthStatus::Degraded);
        assert_eq!(
            health.last_failure().map(SourceFailure::code),
            Some(SafeErrorCode::OtlpJsonDecodeFailed)
        );
    }

    #[test]
    fn stale_health_events_cannot_regress_status_or_last_failure() {
        let mut health = SourceHealth::new(source(), [], at(1)).unwrap();
        health.mark_accepted(at(30));
        health.mark_degraded(SourceFailure::new(
            SafeErrorCode::OtlpJsonDecodeFailed,
            at(10),
        ));

        assert_eq!(health.status(), SourceHealthStatus::Healthy);
        assert_eq!(health.updated_at(), at(30));
        assert_eq!(health.last_failure().unwrap().occurred_at(), at(10));

        health.mark_unavailable(SourceFailure::new(SafeErrorCode::SourceUnavailable, at(40)));
        health.mark_degraded(SourceFailure::new(
            SafeErrorCode::OtlpProtobufDecodeFailed,
            at(35),
        ));

        assert_eq!(health.status(), SourceHealthStatus::Unavailable);
        assert_eq!(health.updated_at(), at(40));
        assert_eq!(
            health.last_failure().map(SourceFailure::code),
            Some(SafeErrorCode::SourceUnavailable)
        );
    }

    #[test]
    fn runtime_availability_does_not_invent_accepted_telemetry() {
        let mut health = SourceHealth::new(source(), [], at(1)).unwrap();
        health.mark_unavailable(SourceFailure::new(SafeErrorCode::OtlpBindFailed, at(2)));
        health.mark_available(at(3));

        assert_eq!(health.status(), SourceHealthStatus::Healthy);
        assert_eq!(health.last_accepted_at(), None);
        assert_eq!(
            health.last_failure().map(SourceFailure::code),
            Some(SafeErrorCode::OtlpBindFailed)
        );
    }

    #[test]
    fn source_capabilities_have_a_hard_cardinality_bound() {
        let fields = (0..=MAX_SOURCE_FIELDS)
            .map(|index| FieldId::new(format!("synthetic.field_{index}")).unwrap());

        assert_eq!(
            SourceHealth::new(source(), fields, at(1)),
            Err(ValidationError::TooManySourceFields {
                max: MAX_SOURCE_FIELDS,
                actual: MAX_SOURCE_FIELDS + 1,
            })
        );
    }
}
