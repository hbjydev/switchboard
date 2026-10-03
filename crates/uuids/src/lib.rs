use uuid::Uuid;

macro_rules! impl_id_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(Uuid);

        #[expect(
            clippy::new_without_default,
            reason = "identity creation must be explicit"
        )]
        impl $name {
            pub fn new() -> Self {
                $name(Uuid::now_v7())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                $name(uuid)
            }
        }

        impl TryFrom<String> for $name {
            type Error = uuid::Error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                let uuid = Uuid::parse_str(&value)?;
                Ok($name(uuid))
            }
        }
    };
}

impl_id_newtype!(PeerId);
impl_id_newtype!(ConversationId);
impl_id_newtype!(MessageId);
impl_id_newtype!(EventId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_uuid_and_text() -> Result<(), uuid::Error> {
        macro_rules! check {
            ($name:ident) => {{
                let id = $name::new();
                assert_eq!($name::from(Uuid::from(id)), id);
                assert_eq!(id.to_string().parse::<$name>()?, id);
                assert_eq!($name::try_from(id.to_string())?, id);
                assert!("invalid".parse::<$name>().is_err());
            }};
        }
        check!(PeerId);
        check!(ConversationId);
        check!(MessageId);
        check!(EventId);
        Ok(())
    }
}
