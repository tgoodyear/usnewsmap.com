-- Run once before deploying a worker with transcription tasks. Same statements as the end of schema.sql,
-- all idempotent, and nothing that exists is touched: no answer is changed or removed.
CREATE TABLE IF NOT EXISTS readings (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  text TEXT NOT NULL,
  unreadable INTEGER NOT NULL DEFAULT 0,
  at INTEGER NOT NULL,
  PRIMARY KEY (task_id, contributor)
);
CREATE INDEX IF NOT EXISTS readings_by_contributor ON readings (contributor);
CREATE TABLE IF NOT EXISTS truths (task_id TEXT PRIMARY KEY, text TEXT NOT NULL, unreadable INTEGER NOT NULL DEFAULT 0, method TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS calibration (task_id TEXT PRIMARY KEY, truth TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS reader_scores (contributor TEXT NOT NULL, lang TEXT NOT NULL, task_id TEXT NOT NULL, cer REAL NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (contributor, task_id));
