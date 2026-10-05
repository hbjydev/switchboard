-- Each claim has a durable fencing identity. Preserve all previous attempts.
CREATE TYPE attempt_state AS ENUM ('Claimed','Running','Completed','Failed','Expired','Cancelled');
CREATE TABLE execution_attempts (
    id uuid PRIMARY KEY,
    issue_id uuid NOT NULL REFERENCES issues(id),
    peer_id uuid NOT NULL REFERENCES peers(id),
    state attempt_state NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    started_at timestamptz,
    heartbeat_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    lease_expires_at timestamptz NOT NULL,
    finished_at timestamptz,
    UNIQUE (issue_id, id),
    CHECK ((state IN ('Claimed','Running')) = (finished_at IS NULL))
);
CREATE UNIQUE INDEX one_active_attempt ON execution_attempts(issue_id)
    WHERE state IN ('Claimed','Running');
CREATE INDEX expiring_attempts ON execution_attempts(lease_expires_at)
    WHERE state IN ('Claimed','Running');
CREATE INDEX attempt_history ON execution_attempts(issue_id,created_at,id);
ALTER TABLE issues ADD COLUMN current_attempt_id uuid;
ALTER TABLE issues ADD CONSTRAINT current_attempt_belongs_to_issue
    FOREIGN KEY (id,current_attempt_id) REFERENCES execution_attempts(issue_id,id);
-- Existing stranded claims become immediately recoverable without losing history.
INSERT INTO execution_attempts(id,issue_id,peer_id,state,created_at,started_at,lease_expires_at)
SELECT gen_random_uuid(),id,owner,status::text::attempt_state,updated_at,
    CASE WHEN status='Running' THEN updated_at END,clock_timestamp()
FROM issues WHERE status IN ('Claimed','Running');
UPDATE issues i SET current_attempt_id=a.id FROM execution_attempts a
WHERE a.issue_id=i.id;
INSERT INTO issue_events(issue_id,actor,event_type,metadata)
SELECT issue_id,peer_id,'ExecutionClaimed',
    jsonb_build_object('attempt_id',id,'lease_expires_at',lease_expires_at,'migration',true)
FROM execution_attempts;
ALTER TABLE issues ADD CONSTRAINT executing_issue_has_attempt
    CHECK ((status IN ('Claimed','Running')) = (current_attempt_id IS NOT NULL));
