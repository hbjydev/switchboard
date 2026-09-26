use uuid::Uuid;

macro_rules! impl_id_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                $name(Uuid::now_v7())
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
