-- Words whose readings look alike in the visitor's font (#1939): lets a task be marked confusable_display, its existing
-- answers set aside in answers_excluded, and the task held out of the queue until it is reopened. Only adds: a Worker from
-- before this change keeps working on the migrated database, so run it BEFORE the merge that deploys the Worker that needs it.
-- Run once (`wrangler d1 execute usnm-check --remote --file migrations/0002-confusable-display.sql` or the D1 query API).
ALTER TABLE tasks ADD COLUMN confusable_display INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN held INTEGER NOT NULL DEFAULT 0;
CREATE TABLE IF NOT EXISTS answers_excluded (
  task_id TEXT NOT NULL,
  contributor TEXT NOT NULL,
  choice TEXT NOT NULL CHECK (choice IN ('a', 'b', 'neither', 'unsure', 'notext')),
  at INTEGER NOT NULL,
  PRIMARY KEY (task_id, contributor)
);
