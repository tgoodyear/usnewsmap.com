-- What volunteers type for a word they judged neither reading right ("What does it say?", README.md, "Suggestions"). A new
-- table only: no existing table is altered, so the Worker from before this change keeps working on the migrated database, and
-- this must run BEFORE the merge that deploys the Worker with POST /api/suggestions (its inserts fail without the table).
-- Suggestions are extra evidence; nothing reads this table to settle a word or count an answer.
-- Run once (`wrangler d1 execute usnm-check --remote --file migrations/0007-suggestions.sql` or the D1 query API).
CREATE TABLE IF NOT EXISTS suggestions (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,      -- the same session id as answers.contributor
  text TEXT NOT NULL,             -- trimmed, NFC, 1 to 200 characters, no control characters
  at INTEGER NOT NULL,            -- unix milliseconds
  PRIMARY KEY (task_id, contributor)
);
