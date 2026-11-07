-- Lets answers.choice be 'notext' on a database made before it existed (#1632). SQLite can't change a
-- CHECK, so this copies answers into a new table and swaps it in. Run once, before deploying a page that
-- offers "Not text", as one batch (`wrangler d1 execute usnm-check --remote --file ...` or the D1 query
-- API), which D1 runs as one transaction: the third statement fails, undoing all of it, if a row is lost.
-- Applied to usnm-check on 7 Nov 2026 (614 answers before and after).
CREATE TABLE answers_new (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')),
  at INTEGER NOT NULL,
  PRIMARY KEY (task_id, contributor)
);
INSERT INTO answers_new (task_id, contributor, choice, at) SELECT task_id, contributor, choice, at FROM answers;
SELECT CASE WHEN (SELECT COUNT(*) FROM answers_new) <> (SELECT COUNT(*) FROM answers) THEN json_extract('row counts differ', '$') END;
DROP TABLE answers;
ALTER TABLE answers_new RENAME TO answers;
CREATE INDEX answers_by_contributor ON answers (contributor);
