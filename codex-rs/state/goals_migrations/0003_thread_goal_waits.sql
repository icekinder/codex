CREATE TABLE thread_goal_waits (
    thread_id TEXT PRIMARY KEY NOT NULL REFERENCES thread_goals(thread_id) ON DELETE CASCADE,
    goal_id TEXT NOT NULL,
    state_key TEXT NOT NULL,
    generation INTEGER NOT NULL,
    waiting INTEGER NOT NULL CHECK (waiting IN (0, 1))
);
