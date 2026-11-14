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
  settled INTEGER NOT NULL DEFAULT 0,
  confusable_display INTEGER NOT NULL DEFAULT 0,  -- 1: the readings looked alike in the visitor's font; answers given before are in answers_excluded
  held INTEGER NOT NULL DEFAULT 0                 -- 1: kept out of the queue and closed to answers until reopened (confusable_display only)
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

-- Answers set aside for a task marked confusable_display: kept and exported, never counted (README.md, "Words whose readings look alike").
CREATE TABLE IF NOT EXISTS answers_excluded (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')),
  at INTEGER NOT NULL,
  PRIMARY KEY (task_id, contributor)
);
