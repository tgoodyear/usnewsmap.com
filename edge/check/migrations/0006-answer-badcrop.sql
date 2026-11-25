-- Lets answers.choice and answers_excluded.choice be 'badcrop' ("Bad crop": the box shows more than one line, cuts off the word or
-- marks the wrong thing; README.md, "Bad crop"). SQLite can't change a CHECK, so this copies each table into a new one with the
-- wider CHECK and swaps it in, as 0001 did. Every column and row is kept (task_id, contributor, choice, at, font, names_shown).
-- Run once, as one batch (`wrangler d1 execute usnm-check --remote --file migrations/0006-answer-badcrop.sql` or the D1 query
-- API), which D1 runs as one transaction: a count check fails, undoing all of it, if a row is lost. It changes no column, so a
-- Worker from before this change keeps working on the migrated database: run it BEFORE the merge that deploys the page with
-- "Bad crop". The new Worker's badcrop answers fail the CHECK without it. tasks.outcome and tasks.hold_reason have no CHECK, so
-- `bad_crop` needs nothing there.
CREATE TABLE answers_new (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext', 'badcrop')),
  at INTEGER NOT NULL,
  font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown')),
  names_shown TEXT NOT NULL DEFAULT 'unknown' CHECK (names_shown IN ('shown', 'not_shown', 'unknown')),
  PRIMARY KEY (task_id, contributor)
);
INSERT INTO answers_new (task_id, contributor, choice, at, font, names_shown) SELECT task_id, contributor, choice, at, font, names_shown FROM answers;
SELECT CASE WHEN (SELECT COUNT(*) FROM answers_new) <> (SELECT COUNT(*) FROM answers) THEN json_extract('answers row counts differ', '$') END;
DROP TABLE answers;
ALTER TABLE answers_new RENAME TO answers;
CREATE INDEX answers_by_contributor ON answers (contributor);
CREATE TABLE answers_excluded_new (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext', 'badcrop')),
  at INTEGER NOT NULL,
  font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown')),
  names_shown TEXT NOT NULL DEFAULT 'unknown' CHECK (names_shown IN ('shown', 'not_shown', 'unknown')),
  PRIMARY KEY (task_id, contributor)
);
INSERT INTO answers_excluded_new (task_id, contributor, choice, at, font, names_shown) SELECT task_id, contributor, choice, at, font, names_shown FROM answers_excluded;
SELECT CASE WHEN (SELECT COUNT(*) FROM answers_excluded_new) <> (SELECT COUNT(*) FROM answers_excluded) THEN json_extract('answers_excluded row counts differ', '$') END;
DROP TABLE answers_excluded;
ALTER TABLE answers_excluded_new RENAME TO answers_excluded;
