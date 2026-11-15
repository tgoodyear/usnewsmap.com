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
  held INTEGER NOT NULL DEFAULT 0,                -- 1: kept out of the queue and closed to answers until reopened (a merged_line task is never reopened)
  mark_reason TEXT CHECK (mark_reason IN ('confusable_display', 'merged_line')),  -- why the task was marked and its answers set aside; NULL if never marked (a task marked before migrations/0005-mark-reason.sql has confusable_display = 1 and NULL here)
  outcome_withdrawn TEXT,         -- the outcome a settled task had when it was marked; outcome is NULL from then on
  withdrawn_reason TEXT,          -- the mark_reason it was withdrawn for
  withdrawn_at INTEGER,           -- unix milliseconds
  hold_reason TEXT                -- why a task is held without a mark (action hold): a slug of lower-case letters, digits and hyphens; NULL otherwise
);
CREATE INDEX IF NOT EXISTS tasks_open ON tasks (lang, settled, n);

CREATE TABLE IF NOT EXISTS answers (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,      -- random id from a session, or "seed:<name>" for imported answers
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')),
  at INTEGER NOT NULL,            -- unix milliseconds
  font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown')),  -- the bundled font as the page reported it when the answer was given; unknown for answers from before (migrations/0003-answer-font.sql)
  names_shown TEXT NOT NULL DEFAULT 'unknown' CHECK (names_shown IN ('shown', 'not_shown', 'unknown')),  -- whether letter names were showing, as the page reported it; unknown for answers from before (migrations/0004-answer-names.sql)
  PRIMARY KEY (task_id, contributor)
);
CREATE INDEX IF NOT EXISTS answers_by_contributor ON answers (contributor);

-- Answers set aside for a marked task (mark_reason): kept and exported, never counted (README.md, "Words whose readings look alike").
CREATE TABLE IF NOT EXISTS answers_excluded (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')),
  at INTEGER NOT NULL,
  font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown')),
  names_shown TEXT NOT NULL DEFAULT 'unknown' CHECK (names_shown IN ('shown', 'not_shown', 'unknown')),
  PRIMARY KEY (task_id, contributor)
);
