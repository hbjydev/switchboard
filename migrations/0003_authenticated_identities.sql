-- Authentication binds the provider's immutable (issuer, subject) identity to
-- a durable Peer. Tokens, OAuth client secrets and sessions are never persisted.
CREATE TABLE authenticated_identities (
    issuer text NOT NULL CHECK (length(trim(issuer)) > 0),
    subject text NOT NULL CHECK (length(trim(subject)) > 0),
    peer_id uuid NOT NULL UNIQUE REFERENCES peers(id),
    PRIMARY KEY (issuer, subject)
);
