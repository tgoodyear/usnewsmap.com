-- Why a task was marked (#1978): `confusable_display` as before, or the new `merged_line` (the clip covers two printed lines).
-- A marked task's answers are set aside and it is held; a settled task also keeps the outcome it had in outcome_withdrawn, with
-- the reason and the time. Only adds five nullable columns (the fifth, hold_reason, is for a word held without a mark): a Worker from before this change keeps working on the migrated
-- database, so run it BEFORE the merge that deploys the Worker that needs it. The new Worker fails without it. Tasks already
-- marked confusable_display keep mark_reason NULL and read as confusable_display.
-- Run once (`wrangler d1 execute usnm-check --remote --file migrations/0005-mark-reason.sql` or the D1 query API).
ALTER TABLE tasks ADD COLUMN mark_reason TEXT CHECK (mark_reason IN ('confusable_display', 'merged_line'));
ALTER TABLE tasks ADD COLUMN outcome_withdrawn TEXT;
ALTER TABLE tasks ADD COLUMN withdrawn_reason TEXT;
ALTER TABLE tasks ADD COLUMN withdrawn_at INTEGER;
ALTER TABLE tasks ADD COLUMN hold_reason TEXT;
