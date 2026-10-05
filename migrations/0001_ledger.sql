CREATE TYPE peer_kind AS ENUM ('Human', 'Agent');
CREATE TYPE issue_kind AS ENUM ('Task', 'Question', 'Approval');
CREATE TYPE issue_status AS ENUM ('Backlog','Ready','Claimed','Running','Blocked','WaitingForHuman','Completed','Failed','Cancelled');
CREATE TABLE peers (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
    kind peer_kind NOT NULL
);
CREATE TABLE issues (
    id uuid PRIMARY KEY,
    title text NOT NULL CHECK (length(trim(title)) > 0),
    description text NOT NULL DEFAULT '',
    kind issue_kind NOT NULL,
    status issue_status NOT NULL,
    created_by uuid NOT NULL REFERENCES peers(id),
    owner uuid REFERENCES peers(id),
    parent_id uuid REFERENCES issues(id),
    priority integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (parent_id IS DISTINCT FROM id),
    CHECK (status NOT IN ('Claimed','Running') OR owner IS NOT NULL),
    CHECK (kind = 'Task' OR status IN ('WaitingForHuman','Completed','Cancelled'))
);
CREATE INDEX ready_issues ON issues(priority DESC, created_at, id) WHERE status = 'Ready';
CREATE INDEX child_issues ON issues(parent_id);
CREATE TABLE issue_dependencies (
    issue_id uuid NOT NULL REFERENCES issues(id),
    dependency_id uuid NOT NULL REFERENCES issues(id),
    PRIMARY KEY (issue_id, dependency_id),
    CHECK (issue_id <> dependency_id)
);
CREATE INDEX dependency_dependents ON issue_dependencies(dependency_id);
CREATE TABLE issue_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    issue_id uuid NOT NULL REFERENCES issues(id),
    event_type text NOT NULL,
    actor uuid REFERENCES peers(id),
    occurred_at timestamptz NOT NULL DEFAULT now(),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX issue_history ON issue_events(issue_id, id);
CREATE FUNCTION reject_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'issue history is append-only';
END;
$$;
CREATE TRIGGER immutable_issue_events BEFORE UPDATE OR DELETE ON issue_events
    FOR EACH ROW EXECUTE FUNCTION reject_event_mutation();
