-- check.usnewsmap.com: words where two OCR readings disagree, and what people say the print shows.
-- Applied with `wrangler d1 execute usnm-check --remote --file schema.sql`. Statements are idempotent.

CREATE TABLE IF NOT EXISTS languages (
  code TEXT PRIMARY KEY,          -- BCP 47, e.g. "chr"
  config TEXT NOT NULL            -- JSON: name, intro, font, notes, examples (languages.md in README)
);

CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,            -- "<lang>/<set>/<line>~<word>"
  lang TEXT NOT NULL,
  set_name TEXT NOT NULL,         -- e.g. "phoenix-nt-v1"
  body TEXT NOT NULL,             -- JSON shown to people: page, source, clips, box, a, b, diffs
  n INTEGER NOT NULL DEFAULT 0,   -- answers so far
  outcome TEXT,                   -- a, b, neither, unsure or notext once settled; "disputed" if never agreed
  settled INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS tasks_open ON tasks (lang, settled, n);

CREATE TABLE IF NOT EXISTS answers (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,      -- random id from a session, or "seed:<name>" for imported answers
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')),
  at INTEGER NOT NULL,            -- unix milliseconds
  PRIMARY KEY (task_id, contributor)
);
CREATE INDEX IF NOT EXISTS answers_by_contributor ON answers (contributor);

-- Transcription tasks (kind "transcribe" in a language's configuration): people type what the print
-- shows, with no machine reading in front of them. Two matching readings settle a task; two that
-- differ go to a third person to adjudicate. Calibration tasks carry a known text.
CREATE TABLE IF NOT EXISTS readings (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  text TEXT NOT NULL,             -- as typed, bidi marks removed, line ends LF; "" only with unreadable
  unreadable INTEGER NOT NULL DEFAULT 0,
  at INTEGER NOT NULL,
  PRIMARY KEY (task_id, contributor)
);
CREATE INDEX IF NOT EXISTS readings_by_contributor ON readings (contributor);

CREATE TABLE IF NOT EXISTS truths (
  task_id TEXT PRIMARY KEY,
  text TEXT NOT NULL,
  unreadable INTEGER NOT NULL DEFAULT 0,
  method TEXT NOT NULL,           -- agreed or adjudicated
  at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS calibration (
  task_id TEXT PRIMARY KEY,
  truth TEXT NOT NULL             -- known text from a human source; never sent to the page
);

CREATE TABLE IF NOT EXISTS reader_scores (
  contributor TEXT NOT NULL,
  lang TEXT NOT NULL,
  task_id TEXT NOT NULL,
  cer REAL NOT NULL,              -- edit distance to the known text over its length
  at INTEGER NOT NULL,
  PRIMARY KEY (contributor, task_id)
);
