-- What volunteers write about a word, apart from their answer and any suggestion (README.md, "Notes"). A new table only: no
-- existing table is altered, so the Worker from before this change keeps working on the migrated database. The Worker with
-- POST /api/notes also runs this same CREATE TABLE IF NOT EXISTS itself, so the order of migration and deploy does not matter.
-- Notes are extra evidence; nothing reads this table to settle a word or count an answer.
-- Run once (`wrangler d1 execute usnm-check --remote --file migrations/0008-notes.sql` or the D1 query API).
CREATE TABLE IF NOT EXISTS notes (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,      -- the same session id as answers.contributor
  text TEXT NOT NULL,             -- trimmed, NFC, 1 to 1000 characters, line breaks allowed, no other control characters
  at INTEGER NOT NULL,            -- unix milliseconds
  PRIMARY KEY (task_id, contributor)
);
