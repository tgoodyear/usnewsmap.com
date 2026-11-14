-- Whether the page was showing letter names under the letters when an answer was given: `shown`, `not_shown` (the option was off,
-- or the language has no letter names to show) or `unknown`. Answers already stored are `unknown`, and so is any answer from a page
-- that sends no value. Only adds: a Worker from before this change keeps working on the migrated database (its inserts leave
-- the column at its default), so run it BEFORE the merge that deploys the Worker that needs it. The new Worker fails without it.
-- Run once (`wrangler d1 execute usnm-check --remote --file migrations/0004-answer-names.sql` or the D1 query API).
ALTER TABLE answers ADD COLUMN names_shown TEXT NOT NULL DEFAULT 'unknown' CHECK (names_shown IN ('shown', 'not_shown', 'unknown'));
ALTER TABLE answers_excluded ADD COLUMN names_shown TEXT NOT NULL DEFAULT 'unknown' CHECK (names_shown IN ('shown', 'not_shown', 'unknown'));
