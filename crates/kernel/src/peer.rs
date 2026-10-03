use switchboard_uuids::PeerId;

#[derive(Debug, Clone)]
pub struct Peer {
    id: PeerId,
    display_name: String,
    kind: PeerKind,
    aliases: Vec<PeerAlias>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    Human,
    Agent,
    Service,
}

impl Peer {
    pub fn new(
        display_name: impl Into<String>,
        kind: PeerKind,
    ) -> Result<Self, crate::DomainError> {
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(crate::DomainError::BlankDisplayName);
        }
        Ok(Self {
            id: PeerId::new(),
            display_name,
            kind,
            aliases: Vec::new(),
        })
    }

    #[must_use]
    pub const fn id(&self) -> PeerId {
        self.id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub const fn kind(&self) -> PeerKind {
        self.kind
    }

    pub fn rename(&mut self, display_name: impl Into<String>) -> Result<(), crate::DomainError> {
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(crate::DomainError::BlankDisplayName);
        }
        self.display_name = display_name;
        Ok(())
    }

    /// Attaches an alias locally. Cross-peer uniqueness is checked by the repository.
    pub fn add_alias(&mut self, alias: PeerAlias) {
        if !self.aliases.contains(&alias) {
            self.aliases.push(alias);
        }
    }

    #[must_use]
    pub fn aliases(&self) -> &[PeerAlias] {
        &self.aliases
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerAlias {
    pub provider: String,
    pub scope: String,
    pub external_id: String,
}
