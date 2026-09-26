use switchboard_uuids::PeerId;

pub struct Peer {
    id: PeerId,
    display_name: String,
    kind: PeerKind,
    aliases: Vec<PeerAlias>,
}

pub enum PeerKind {
    Human,
    Agent,
    Service,
}

impl Peer {
    pub fn new(display_name: impl Into<String>, kind: PeerKind) -> Self {
        Self {
            id: PeerId::new(),
            display_name: display_name.into(),
            kind,
            aliases: Vec::new(),
        }
    }

    pub fn id(&self) -> &PeerId {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn kind(&self) -> &PeerKind {
        &self.kind
    }

    pub fn aliases(&self) -> &Vec<PeerAlias> {
        &self.aliases
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAlias {
    pub provider: String,
    pub external_id: String,
}
