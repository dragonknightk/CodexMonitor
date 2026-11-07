use serde::{Deserialize, Serialize};

use crate::{
    CanonicalField, ClientName, ConversationId, ConversationSourceId, EventName, FieldProvenance,
    ModelCallId, ModelCallObservationId, ModelName, Observed, ReasoningEffort, SourceEventIdentity,
    SourceIdentity, StreamEventKind, SurfaceName, ThreadSourceId, TokenCount, TurnId, TurnSourceId,
    UtcTimestamp, ValidationError,
};

/// Whether a model call may enter the realtime action pipeline.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataOrigin {
    Realtime,
    Historical,
}

/// Proven canonical relationship of a model call.
///
/// A turn relationship stores its parent conversation atomically, so field
/// precedence cannot combine a turn from one conversation with another
/// conversation selected from different evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelCallRelationship {
    Conversation {
        conversation_id: ConversationId,
    },
    Turn {
        conversation_id: ConversationId,
        turn_id: TurnId,
    },
}

impl ModelCallRelationship {
    /// Convert separately extracted optional relationships into an atomic value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TurnWithoutConversation`] when `turn_id` is
    /// present but `conversation_id` is absent.
    pub fn from_parts(
        conversation_id: Option<ConversationId>,
        turn_id: Option<TurnId>,
    ) -> Result<Option<Self>, ValidationError> {
        match (conversation_id, turn_id) {
            (None, None) => Ok(None),
            (Some(conversation_id), None) => Ok(Some(Self::Conversation { conversation_id })),
            (Some(conversation_id), Some(turn_id)) => Ok(Some(Self::Turn {
                conversation_id,
                turn_id,
            })),
            (None, Some(_)) => Err(ValidationError::TurnWithoutConversation),
        }
    }

    /// Return the required conversation side of the relationship.
    #[must_use]
    pub const fn conversation_id(self) -> ConversationId {
        match self {
            Self::Conversation { conversation_id }
            | Self::Turn {
                conversation_id, ..
            } => conversation_id,
        }
    }

    /// Return the turn only when a source proved the complete relationship.
    #[must_use]
    pub const fn turn_id(self) -> Option<TurnId> {
        match self {
            Self::Conversation { .. } => None,
            Self::Turn { turn_id, .. } => Some(turn_id),
        }
    }
}

/// A canonical Codex conversation container.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Conversation {
    id: ConversationId,
    source_id: Option<CanonicalField<ConversationSourceId>>,
    thread_id: Option<CanonicalField<ThreadSourceId>>,
    first_observed_at: UtcTimestamp,
    last_observed_at: UtcTimestamp,
}

impl Conversation {
    /// Create a conversation without inventing an unavailable source ID.
    #[must_use]
    pub const fn new(
        id: ConversationId,
        source_id: Option<CanonicalField<ConversationSourceId>>,
        thread_id: Option<CanonicalField<ThreadSourceId>>,
        observed_at: UtcTimestamp,
    ) -> Self {
        Self {
            id,
            source_id,
            thread_id,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
        }
    }

    /// Extend the observed lifetime without moving either boundary backwards.
    pub fn observe_at(&mut self, observed_at: UtcTimestamp) {
        self.first_observed_at = self.first_observed_at.min(observed_at);
        self.last_observed_at = self.last_observed_at.max(observed_at);
    }

    /// Return the internal conversation ID.
    #[must_use]
    pub const fn id(&self) -> ConversationId {
        self.id
    }

    /// Return the canonical source conversation ID, if a source supplied one.
    #[must_use]
    pub const fn source_id(&self) -> Option<&CanonicalField<ConversationSourceId>> {
        self.source_id.as_ref()
    }

    /// Return the canonical Codex thread identifier, if known.
    #[must_use]
    pub const fn thread_id(&self) -> Option<&CanonicalField<ThreadSourceId>> {
        self.thread_id.as_ref()
    }

    /// Merge a newly observed source conversation identifier.
    pub fn merge_source_id(&mut self, candidate: Observed<ConversationSourceId>) {
        merge_optional(&mut self.source_id, candidate);
    }

    /// Merge a newly observed Codex thread identifier.
    pub fn merge_thread_id(&mut self, candidate: Observed<ThreadSourceId>) {
        merge_optional(&mut self.thread_id, candidate);
    }

    /// Return the first observed UTC instant.
    #[must_use]
    pub const fn first_observed_at(&self) -> UtcTimestamp {
        self.first_observed_at
    }

    /// Return the latest observed UTC instant.
    #[must_use]
    pub const fn last_observed_at(&self) -> UtcTimestamp {
        self.last_observed_at
    }
}

/// A user-visible turn that belongs to one canonical conversation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Turn {
    id: TurnId,
    conversation_id: ConversationId,
    source_id: Option<CanonicalField<TurnSourceId>>,
    event_time: Option<CanonicalField<UtcTimestamp>>,
    first_observed_at: UtcTimestamp,
    last_observed_at: UtcTimestamp,
}

impl Turn {
    /// Create a turn linked to its required conversation.
    #[must_use]
    pub const fn new(
        id: TurnId,
        conversation_id: ConversationId,
        source_id: Option<CanonicalField<TurnSourceId>>,
        event_time: Option<CanonicalField<UtcTimestamp>>,
        observed_at: UtcTimestamp,
    ) -> Self {
        Self {
            id,
            conversation_id,
            source_id,
            event_time,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
        }
    }

    /// Extend the observed lifetime without moving either boundary backwards.
    pub fn observe_at(&mut self, observed_at: UtcTimestamp) {
        self.first_observed_at = self.first_observed_at.min(observed_at);
        self.last_observed_at = self.last_observed_at.max(observed_at);
    }

    /// Return the internal turn ID.
    #[must_use]
    pub const fn id(&self) -> TurnId {
        self.id
    }

    /// Return the required parent conversation ID.
    #[must_use]
    pub const fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    /// Return the canonical source turn ID, if known.
    #[must_use]
    pub const fn source_id(&self) -> Option<&CanonicalField<TurnSourceId>> {
        self.source_id.as_ref()
    }

    /// Return the proven turn event time, if known.
    #[must_use]
    pub const fn event_time(&self) -> Option<&CanonicalField<UtcTimestamp>> {
        self.event_time.as_ref()
    }

    /// Merge a newly observed source turn identifier.
    pub fn merge_source_id(&mut self, candidate: Observed<TurnSourceId>) {
        merge_optional(&mut self.source_id, candidate);
    }

    /// Merge a newly observed turn event time.
    pub fn merge_event_time(&mut self, candidate: Observed<UtcTimestamp>) {
        merge_optional(&mut self.event_time, candidate);
    }

    /// Return the earliest local observation time.
    #[must_use]
    pub const fn first_observed_at(&self) -> UtcTimestamp {
        self.first_observed_at
    }

    /// Return the latest local observation time.
    #[must_use]
    pub const fn last_observed_at(&self) -> UtcTimestamp {
        self.last_observed_at
    }
}

/// Source fields that may contribute to one canonical model call.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationFields {
    pub relationship: Option<Observed<ModelCallRelationship>>,
    pub conversation_source_id: Option<Observed<ConversationSourceId>>,
    pub thread_source_id: Option<Observed<ThreadSourceId>>,
    pub turn_source_id: Option<Observed<TurnSourceId>>,
    pub model: Option<Observed<ModelName>>,
    pub reasoning_effort: Option<Observed<ReasoningEffort>>,
    pub input_tokens: Option<Observed<TokenCount>>,
    pub cached_input_tokens: Option<Observed<TokenCount>>,
    pub output_tokens: Option<Observed<TokenCount>>,
    pub reasoning_output_tokens: Option<Observed<TokenCount>>,
    pub surface: Option<Observed<SurfaceName>>,
    pub client: Option<Observed<ClientName>>,
    pub event_name: Option<Observed<EventName>>,
    pub stream_event_kind: Option<Observed<StreamEventKind>>,
}

impl ObservationFields {
    fn provenances(&self) -> impl Iterator<Item = &FieldProvenance> {
        [
            self.relationship.as_ref().map(Observed::provenance),
            self.conversation_source_id
                .as_ref()
                .map(Observed::provenance),
            self.thread_source_id.as_ref().map(Observed::provenance),
            self.turn_source_id.as_ref().map(Observed::provenance),
            self.model.as_ref().map(Observed::provenance),
            self.reasoning_effort.as_ref().map(Observed::provenance),
            self.input_tokens.as_ref().map(Observed::provenance),
            self.cached_input_tokens.as_ref().map(Observed::provenance),
            self.output_tokens.as_ref().map(Observed::provenance),
            self.reasoning_output_tokens
                .as_ref()
                .map(Observed::provenance),
            self.surface.as_ref().map(Observed::provenance),
            self.client.as_ref().map(Observed::provenance),
            self.event_name.as_ref().map(Observed::provenance),
            self.stream_event_kind.as_ref().map(Observed::provenance),
        ]
        .into_iter()
        .flatten()
    }

    /// Return whether every present field carries provenance from `source`.
    #[must_use]
    pub fn all_from(&self, source: &SourceIdentity) -> bool {
        self.provenances()
            .all(|provenance| provenance.source() == source)
    }
}

/// Canonical fields projected from all retained source observations.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CanonicalModelCallFields {
    pub relationship: Option<CanonicalField<ModelCallRelationship>>,
    pub conversation_source_id: Option<CanonicalField<ConversationSourceId>>,
    pub thread_source_id: Option<CanonicalField<ThreadSourceId>>,
    pub turn_source_id: Option<CanonicalField<TurnSourceId>>,
    pub model: Option<CanonicalField<ModelName>>,
    pub reasoning_effort: Option<CanonicalField<ReasoningEffort>>,
    pub input_tokens: Option<CanonicalField<TokenCount>>,
    pub cached_input_tokens: Option<CanonicalField<TokenCount>>,
    pub output_tokens: Option<CanonicalField<TokenCount>>,
    pub reasoning_output_tokens: Option<CanonicalField<TokenCount>>,
    pub surface: Option<CanonicalField<SurfaceName>>,
    pub client: Option<CanonicalField<ClientName>>,
    pub event_name: Option<CanonicalField<EventName>>,
    pub stream_event_kind: Option<CanonicalField<StreamEventKind>>,
}

macro_rules! merge_field {
    ($target:expr, $candidate:expr) => {
        if let Some(candidate) = $candidate {
            match $target.as_mut() {
                Some(current) => {
                    current.merge(candidate);
                }
                None => {
                    *$target = Some(CanonicalField::new(candidate));
                }
            }
        }
    };
}

fn merge_optional<T: Ord>(target: &mut Option<CanonicalField<T>>, candidate: Observed<T>) {
    match target.as_mut() {
        Some(current) => {
            current.merge(candidate);
        }
        None => *target = Some(CanonicalField::new(candidate)),
    }
}

impl CanonicalModelCallFields {
    fn merged_with(&self, fields: ObservationFields) -> Self {
        let mut merged = self.clone();
        merge_field!(&mut merged.relationship, fields.relationship);
        merge_field!(
            &mut merged.conversation_source_id,
            fields.conversation_source_id
        );
        merge_field!(&mut merged.thread_source_id, fields.thread_source_id);
        merge_field!(&mut merged.turn_source_id, fields.turn_source_id);
        merge_field!(&mut merged.model, fields.model);
        merge_field!(&mut merged.reasoning_effort, fields.reasoning_effort);
        merge_field!(&mut merged.input_tokens, fields.input_tokens);
        merge_field!(&mut merged.cached_input_tokens, fields.cached_input_tokens);
        merge_field!(&mut merged.output_tokens, fields.output_tokens);
        merge_field!(
            &mut merged.reasoning_output_tokens,
            fields.reasoning_output_tokens
        );
        merge_field!(&mut merged.surface, fields.surface);
        merge_field!(&mut merged.client, fields.client);
        merge_field!(&mut merged.event_name, fields.event_name);
        merge_field!(&mut merged.stream_event_kind, fields.stream_event_kind);
        merged
    }
}

/// One canonical model request, independently queryable from its source records.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelCall {
    id: ModelCallId,
    canonical_identity: SourceEventIdentity,
    event_time: CanonicalField<UtcTimestamp>,
    first_observed_at: UtcTimestamp,
    last_observed_at: UtcTimestamp,
    origin: DataOrigin,
    fields: CanonicalModelCallFields,
}

impl ModelCall {
    /// Create a canonical model call from typed optional fields.
    #[must_use]
    pub fn new(
        id: ModelCallId,
        canonical_identity: SourceEventIdentity,
        event_time: CanonicalField<UtcTimestamp>,
        observed_at: UtcTimestamp,
        origin: DataOrigin,
        fields: CanonicalModelCallFields,
    ) -> Self {
        Self {
            id,
            canonical_identity,
            event_time,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
            origin,
            fields,
        }
    }

    /// Apply one retained source observation to the canonical projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the observation is linked to a different call.
    pub fn apply_observation(
        &mut self,
        observation: &ModelCallObservation,
    ) -> Result<(), ValidationError> {
        if observation.model_call_id() != self.id {
            return Err(ValidationError::ObservationModelCallMismatch);
        }

        let merged = self.fields.merged_with(observation.fields.clone());
        self.event_time.merge(observation.event_time.clone());
        self.fields = merged;
        self.first_observed_at = self.first_observed_at.min(observation.observed_at);
        self.last_observed_at = self.last_observed_at.max(observation.observed_at);
        Ok(())
    }

    /// Return the internal model-call ID.
    #[must_use]
    pub const fn id(&self) -> ModelCallId {
        self.id
    }

    /// Borrow the versioned identity used for canonical deduplication.
    #[must_use]
    pub const fn canonical_identity(&self) -> &SourceEventIdentity {
        &self.canonical_identity
    }

    /// Return the canonical source event time and its selected provenance.
    #[must_use]
    pub const fn event_time(&self) -> &CanonicalField<UtcTimestamp> {
        &self.event_time
    }

    /// Return whether the call is realtime or historical.
    #[must_use]
    pub const fn origin(&self) -> DataOrigin {
        self.origin
    }

    /// Borrow the canonical source-derived fields.
    #[must_use]
    pub const fn fields(&self) -> &CanonicalModelCallFields {
        &self.fields
    }

    /// Return the earliest local observation time.
    #[must_use]
    pub const fn first_observed_at(&self) -> UtcTimestamp {
        self.first_observed_at
    }

    /// Return the latest local observation time.
    #[must_use]
    pub const fn last_observed_at(&self) -> UtcTimestamp {
        self.last_observed_at
    }
}

/// One privacy-reviewed source observation retained for audit and re-projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelCallObservation {
    id: ModelCallObservationId,
    model_call_id: ModelCallId,
    source_event_identity: SourceEventIdentity,
    event_time: Observed<UtcTimestamp>,
    observed_at: UtcTimestamp,
    origin: DataOrigin,
    fields: ObservationFields,
}

impl ModelCallObservation {
    /// Create an observation whose field evidence belongs to the same source.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ObservationSourceMismatch`] when any field
    /// provenance names a source other than the source-event identity.
    pub fn new(
        id: ModelCallObservationId,
        model_call_id: ModelCallId,
        source_event_identity: SourceEventIdentity,
        event_time: Observed<UtcTimestamp>,
        observed_at: UtcTimestamp,
        origin: DataOrigin,
        fields: ObservationFields,
    ) -> Result<Self, ValidationError> {
        if event_time.provenance().source() != source_event_identity.source()
            || !fields.all_from(source_event_identity.source())
        {
            return Err(ValidationError::ObservationSourceMismatch);
        }
        Ok(Self {
            id,
            model_call_id,
            source_event_identity,
            event_time,
            observed_at,
            origin,
            fields,
        })
    }

    /// Return the internal observation ID.
    #[must_use]
    pub const fn id(&self) -> ModelCallObservationId {
        self.id
    }

    /// Return the required correlated model-call ID.
    #[must_use]
    pub const fn model_call_id(&self) -> ModelCallId {
        self.model_call_id
    }

    /// Borrow the deterministic source-event identity.
    #[must_use]
    pub const fn source_event_identity(&self) -> &SourceEventIdentity {
        &self.source_event_identity
    }

    /// Return the source event time and its field provenance.
    #[must_use]
    pub const fn event_time(&self) -> &Observed<UtcTimestamp> {
        &self.event_time
    }

    /// Return when `CodexMonitor` observed the source event.
    #[must_use]
    pub const fn observed_at(&self) -> UtcTimestamp {
        self.observed_at
    }

    /// Return whether the observation is realtime or historical.
    #[must_use]
    pub const fn origin(&self) -> DataOrigin {
        self.origin
    }

    /// Borrow the optional privacy-reviewed fields.
    #[must_use]
    pub const fn fields(&self) -> &ObservationFields {
        &self.fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EvidenceQuality, FallbackFingerprintInput, FingerprintTokenFields, SourceContractVersion,
        SourceId, SourcePriority,
    };

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn source(id: &str) -> SourceIdentity {
        SourceIdentity::new(
            SourceId::new(id).unwrap(),
            SourceContractVersion::new(1).unwrap(),
        )
    }

    fn evidence<T>(value: T, source: SourceIdentity, quality: EvidenceQuality) -> Observed<T> {
        Observed::new(
            value,
            FieldProvenance::new(source, at(10), quality, SourcePriority::new(10)),
        )
    }

    fn identity(source: SourceIdentity) -> SourceEventIdentity {
        SourceEventIdentity::from_fallback(
            source,
            FallbackFingerprintInput {
                event_time: at(9),
                conversation: None,
                model: None,
                tokens: FingerprintTokenFields::default(),
            },
        )
    }

    fn event_time(source: SourceIdentity) -> Observed<UtcTimestamp> {
        evidence(at(9), source, EvidenceQuality::Direct)
    }

    #[test]
    fn conversation_and_turn_keep_typed_utc_lifetimes() {
        let conversation_id = ConversationId::new();
        let mut conversation = Conversation::new(conversation_id, None, None, at(10));
        conversation.observe_at(at(20));
        conversation.observe_at(at(5));

        assert_eq!(conversation.first_observed_at(), at(5));
        assert_eq!(conversation.last_observed_at(), at(20));

        let mut turn = Turn::new(TurnId::new(), conversation_id, None, None, at(10));
        turn.merge_event_time(event_time(source("sessions")));
        turn.observe_at(at(15));

        assert_eq!(turn.event_time().unwrap().value(), &at(9));
        assert_eq!(turn.first_observed_at(), at(10));
        assert_eq!(turn.last_observed_at(), at(15));
    }

    #[test]
    fn missing_turn_relationship_remains_unknown() {
        let source = source("otlp");
        let fields = CanonicalModelCallFields::default();
        let call = ModelCall::new(
            ModelCallId::new(),
            identity(source.clone()),
            CanonicalField::new(event_time(source)),
            at(10),
            DataOrigin::Realtime,
            fields,
        );

        assert!(call.fields().relationship.is_none());
        assert!(call.fields().model.is_none());
    }

    #[test]
    fn a_turn_without_a_conversation_is_rejected() {
        assert_eq!(
            ModelCallRelationship::from_parts(None, Some(TurnId::new())),
            Err(ValidationError::TurnWithoutConversation)
        );
    }

    #[test]
    fn observation_rejects_provenance_from_another_source() {
        let identity_source = source("otlp");
        let fields = ObservationFields {
            model: Some(evidence(
                ModelName::new("gpt-5.6-terra").unwrap(),
                source("sessions"),
                EvidenceQuality::Direct,
            )),
            ..ObservationFields::default()
        };

        assert_eq!(
            ModelCallObservation::new(
                ModelCallObservationId::new(),
                ModelCallId::new(),
                identity(identity_source.clone()),
                event_time(identity_source),
                at(10),
                DataOrigin::Realtime,
                fields,
            ),
            Err(ValidationError::ObservationSourceMismatch)
        );
    }

    #[test]
    fn turn_relationship_and_parent_are_selected_atomically() {
        let call_id = ModelCallId::new();
        let initial_source = source("source-z");
        let initial_conversation = ConversationId::new();
        let competing_conversation = ConversationId::new();
        let competing_turn = TurnId::new();
        let initial_relationship = Observed::new(
            ModelCallRelationship::Conversation {
                conversation_id: initial_conversation,
            },
            FieldProvenance::new(
                initial_source.clone(),
                at(10),
                EvidenceQuality::Direct,
                SourcePriority::new(20),
            ),
        );
        let mut call = ModelCall::new(
            call_id,
            identity(initial_source.clone()),
            CanonicalField::new(event_time(initial_source)),
            at(10),
            DataOrigin::Realtime,
            CanonicalModelCallFields {
                relationship: Some(CanonicalField::new(initial_relationship)),
                ..CanonicalModelCallFields::default()
            },
        );
        let competing_source = source("source-a");
        let observation = ModelCallObservation::new(
            ModelCallObservationId::new(),
            call_id,
            identity(competing_source.clone()),
            event_time(competing_source.clone()),
            at(20),
            DataOrigin::Realtime,
            ObservationFields {
                relationship: Some(Observed::new(
                    ModelCallRelationship::Turn {
                        conversation_id: competing_conversation,
                        turn_id: competing_turn,
                    },
                    FieldProvenance::new(
                        competing_source,
                        at(20),
                        EvidenceQuality::Direct,
                        SourcePriority::new(10),
                    ),
                )),
                ..ObservationFields::default()
            },
        )
        .unwrap();

        call.apply_observation(&observation).unwrap();

        let relationship = *call.fields().relationship.as_ref().unwrap().value();
        assert_eq!(relationship.conversation_id(), initial_conversation);
        assert_eq!(relationship.turn_id(), None);
    }

    #[test]
    fn unknown_optional_values_round_trip_without_invention() {
        let source = source("otlp");
        let call_id = ModelCallId::new();
        let observation = ModelCallObservation::new(
            ModelCallObservationId::new(),
            call_id,
            identity(source.clone()),
            event_time(source),
            at(10),
            DataOrigin::Realtime,
            ObservationFields::default(),
        )
        .unwrap();
        let json = serde_json::to_string(observation.fields()).unwrap();
        let decoded: ObservationFields = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, ObservationFields::default());
        assert_eq!(observation.model_call_id(), call_id);
    }

    #[test]
    fn observation_cannot_project_into_a_different_model_call() {
        let linked_call_id = ModelCallId::new();
        let source = source("otlp");
        let observation = ModelCallObservation::new(
            ModelCallObservationId::new(),
            linked_call_id,
            identity(source.clone()),
            event_time(source.clone()),
            at(10),
            DataOrigin::Realtime,
            ObservationFields::default(),
        )
        .unwrap();
        let mut other_call = ModelCall::new(
            ModelCallId::new(),
            identity(source.clone()),
            CanonicalField::new(event_time(source)),
            at(10),
            DataOrigin::Realtime,
            CanonicalModelCallFields::default(),
        );

        assert_eq!(
            other_call.apply_observation(&observation),
            Err(ValidationError::ObservationModelCallMismatch)
        );
    }
}
