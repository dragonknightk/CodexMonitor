use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{SourceIdentity, SourcePriority, UtcTimestamp};

/// Quality of the evidence supporting a field value.
///
/// The declaration order is intentional: direct evidence always outranks
/// inferred evidence, regardless of source priority or observation time.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceQuality {
    Inferred,
    Direct,
}

/// Provenance retained alongside every source-derived field.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FieldProvenance {
    source: SourceIdentity,
    observed_at: UtcTimestamp,
    quality: EvidenceQuality,
    source_priority: SourcePriority,
}

impl FieldProvenance {
    /// Create field provenance from registered source metadata.
    #[must_use]
    pub const fn new(
        source: SourceIdentity,
        observed_at: UtcTimestamp,
        quality: EvidenceQuality,
        source_priority: SourcePriority,
    ) -> Self {
        Self {
            source,
            observed_at,
            quality,
            source_priority,
        }
    }

    /// Return the source that supplied the value.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// Return when the source observation was accepted.
    #[must_use]
    pub const fn observed_at(&self) -> UtcTimestamp {
        self.observed_at
    }

    /// Return whether the evidence was direct or inferred.
    #[must_use]
    pub const fn quality(&self) -> EvidenceQuality {
        self.quality
    }

    /// Return the configured priority inside the evidence-quality tier.
    #[must_use]
    pub const fn source_priority(&self) -> SourcePriority {
        self.source_priority
    }

    fn precedence(&self, other: &Self) -> Ordering {
        self.quality
            .cmp(&other.quality)
            .then_with(|| self.source_priority.cmp(&other.source_priority))
            .then_with(|| self.source.cmp(&other.source))
    }
}

/// A source-derived value paired with its complete provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observed<T> {
    value: T,
    provenance: FieldProvenance,
}

impl<T> Observed<T> {
    /// Pair a value with its source evidence.
    #[must_use]
    pub const fn new(value: T, provenance: FieldProvenance) -> Self {
        Self { value, provenance }
    }

    /// Borrow the observed value.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Borrow the field provenance.
    #[must_use]
    pub const fn provenance(&self) -> &FieldProvenance {
        &self.provenance
    }

    /// Split the observation into its value and provenance.
    #[must_use]
    pub fn into_parts(self) -> (T, FieldProvenance) {
        (self.value, self.provenance)
    }
}

/// Result of applying evidence to a canonical field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeOutcome {
    KeptCurrent,
    SelectedCandidate,
    Duplicate,
}

/// Canonical field selected from one or more retained source observations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CanonicalField<T> {
    selected: Observed<T>,
}

impl<T: Ord> CanonicalField<T> {
    /// Seed a canonical field with its first known observation.
    #[must_use]
    pub const fn new(selected: Observed<T>) -> Self {
        Self { selected }
    }

    /// Borrow the selected value.
    #[must_use]
    pub const fn value(&self) -> &T {
        self.selected.value()
    }

    /// Borrow the selected provenance.
    #[must_use]
    pub const fn provenance(&self) -> &FieldProvenance {
        self.selected.provenance()
    }

    /// Apply another observation using the stable precedence policy.
    ///
    /// Exact provenance conflicts are resolved by the value's natural ordering.
    /// That final guard makes replay order irrelevant even if a faulty source
    /// emits two values under the same evidence identity.
    pub fn merge(&mut self, candidate: Observed<T>) -> MergeOutcome {
        let ordering = candidate
            .provenance()
            .precedence(self.selected.provenance())
            .then_with(|| candidate.value().cmp(self.selected.value()));

        match ordering {
            Ordering::Greater => {
                self.selected = candidate;
                MergeOutcome::SelectedCandidate
            }
            Ordering::Equal => MergeOutcome::Duplicate,
            Ordering::Less => MergeOutcome::KeptCurrent,
        }
    }

    /// Consume the canonical field and return the selected observation.
    #[must_use]
    pub fn into_selected(self) -> Observed<T> {
        self.selected
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{ModelName, SourceContractVersion, SourceId};

    fn source(id: &str) -> SourceIdentity {
        SourceIdentity::new(
            SourceId::new(id).unwrap(),
            SourceContractVersion::new(1).unwrap(),
        )
    }

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn observed(
        value: &str,
        source_id: &str,
        quality: EvidenceQuality,
        priority: u16,
        seconds: i128,
    ) -> Observed<ModelName> {
        Observed::new(
            ModelName::new(value).unwrap(),
            FieldProvenance::new(
                source(source_id),
                at(seconds),
                quality,
                SourcePriority::new(priority),
            ),
        )
    }

    fn select(values: impl IntoIterator<Item = Observed<ModelName>>) -> CanonicalField<ModelName> {
        let mut values = values.into_iter();
        let mut selected = CanonicalField::new(values.next().unwrap());
        for value in values {
            selected.merge(value);
        }
        selected
    }

    #[test]
    fn direct_evidence_beats_newer_higher_priority_inference() {
        let direct = observed("gpt-direct", "otlp", EvidenceQuality::Direct, 1, 10);
        let inferred = observed("gpt-inferred", "config", EvidenceQuality::Inferred, 500, 20);
        let mut canonical = CanonicalField::new(direct);

        assert_eq!(canonical.merge(inferred), MergeOutcome::KeptCurrent);
        assert_eq!(canonical.value().as_str(), "gpt-direct");
    }

    #[test]
    fn conflict_resolution_is_independent_of_arrival_order() {
        let left = observed("gpt-alpha", "otlp", EvidenceQuality::Direct, 10, 10);
        let right = observed("gpt-beta", "sessions", EvidenceQuality::Direct, 20, 5);

        let mut first = CanonicalField::new(left.clone());
        first.merge(right.clone());
        let mut second = CanonicalField::new(right);
        second.merge(left);

        assert_eq!(first, second);
        assert_eq!(first.value().as_str(), "gpt-beta");
    }

    #[test]
    fn contradictory_values_with_exact_provenance_have_a_stable_tie_break() {
        let left = observed("gpt-alpha", "otlp", EvidenceQuality::Direct, 10, 10);
        let right = observed("gpt-beta", "otlp", EvidenceQuality::Direct, 10, 10);

        let mut left_first = CanonicalField::new(left.clone());
        left_first.merge(right.clone());
        let mut right_first = CanonicalField::new(right);
        right_first.merge(left);

        assert_eq!(left_first, right_first);
        assert_eq!(left_first.value().as_str(), "gpt-beta");
    }

    #[test]
    fn receiver_observation_time_is_audit_data_not_precedence() {
        let stable_winner = observed("gpt-stable", "source-z", EvidenceQuality::Direct, 10, 1);
        let later_arrival = observed("gpt-later", "source-a", EvidenceQuality::Direct, 10, 999);
        let mut canonical = CanonicalField::new(stable_winner);

        canonical.merge(later_arrival);

        assert_eq!(canonical.value().as_str(), "gpt-stable");
    }

    proptest! {
        #[test]
        fn arbitrary_pairwise_merge_is_independent_of_arrival_order(
            left_value in "[A-Za-z0-9][A-Za-z0-9._-]{0,31}",
            right_value in "[A-Za-z0-9][A-Za-z0-9._-]{0,31}",
            left_direct in any::<bool>(),
            right_direct in any::<bool>(),
            left_priority in any::<u16>(),
            right_priority in any::<u16>(),
            left_seconds in 0_i128..1_000_000,
            right_seconds in 0_i128..1_000_000,
        ) {
            let left = observed(
                &left_value,
                "source-a",
                if left_direct { EvidenceQuality::Direct } else { EvidenceQuality::Inferred },
                left_priority,
                left_seconds,
            );
            let right = observed(
                &right_value,
                "source-b",
                if right_direct { EvidenceQuality::Direct } else { EvidenceQuality::Inferred },
                right_priority,
                right_seconds,
            );

            let mut left_first = CanonicalField::new(left.clone());
            left_first.merge(right.clone());
            let mut right_first = CanonicalField::new(right);
            right_first.merge(left);

            prop_assert_eq!(left_first, right_first);
        }

        #[test]
        fn multi_observation_merge_is_commutative_associative_and_idempotent(
            first_priority in any::<u16>(),
            second_priority in any::<u16>(),
            third_priority in any::<u16>(),
            first_direct in any::<bool>(),
            second_direct in any::<bool>(),
            third_direct in any::<bool>(),
        ) {
            let quality = |direct| if direct {
                EvidenceQuality::Direct
            } else {
                EvidenceQuality::Inferred
            };
            let first = observed("model-a", "source-a", quality(first_direct), first_priority, 3);
            let second = observed("model-b", "source-b", quality(second_direct), second_priority, 2);
            let third = observed("model-c", "source-c", quality(third_direct), third_priority, 1);
            let expected = select([first.clone(), second.clone(), third.clone()]);

            for permutation in [
                vec![first.clone(), third.clone(), second.clone()],
                vec![second.clone(), first.clone(), third.clone()],
                vec![second.clone(), third.clone(), first.clone()],
                vec![third.clone(), first.clone(), second.clone()],
                vec![third.clone(), second.clone(), first.clone()],
            ] {
                prop_assert_eq!(select(permutation), expected.clone());
            }

            let mut idempotent = expected.clone();
            let selected = idempotent.clone().into_selected();
            prop_assert_eq!(idempotent.merge(selected), MergeOutcome::Duplicate);
            prop_assert_eq!(idempotent, expected);
        }
    }
}
