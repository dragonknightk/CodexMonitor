use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! domain_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[doc = concat!("Generate a new time-ordered `", stringify!($name), "`.")]
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            #[doc = concat!("Construct a `", stringify!($name), "` from a UUID.")]
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            #[doc = concat!("Return the UUID stored in this `", stringify!($name), "`.")]
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

domain_id!(
    ConversationId,
    "Internal identity for a canonical conversation."
);
domain_id!(TurnId, "Internal identity for a user-visible turn.");
domain_id!(ModelCallId, "Internal identity for one model request.");
domain_id!(
    ModelCallObservationId,
    "Internal identity for one source observation of a model call."
);
domain_id!(AlertId, "Internal identity for one durable rule alert.");
domain_id!(
    ActionId,
    "Internal identity for one durable action request."
);
domain_id!(
    NotificationCoalescingWindowId,
    "Internal identity for one durable notification-coalescing window."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_domain_ids_are_uuid_v7() {
        assert_eq!(ConversationId::new().as_uuid().get_version_num(), 7);
        assert_eq!(TurnId::new().as_uuid().get_version_num(), 7);
        assert_eq!(ModelCallId::new().as_uuid().get_version_num(), 7);
        assert_eq!(ModelCallObservationId::new().as_uuid().get_version_num(), 7);
        assert_eq!(AlertId::new().as_uuid().get_version_num(), 7);
        assert_eq!(ActionId::new().as_uuid().get_version_num(), 7);
        assert_eq!(
            NotificationCoalescingWindowId::new()
                .as_uuid()
                .get_version_num(),
            7
        );
    }
}
