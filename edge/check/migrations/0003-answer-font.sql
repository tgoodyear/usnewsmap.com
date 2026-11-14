-- What the visitor's page reported about the bundled font when an answer was given (#1939): `loaded`, `not_loaded`, `none`
-- (no bundled font applies to the word) or `unknown`. Answers already stored are `unknown`, and so is any answer from a page
-- that sends no value. Only adds: a Worker from before this change keeps working on the migrated database (its inserts leave
-- the column at its default), so run it BEFORE the merge that deploys the Worker that needs it. The new Worker fails without it.
-- Run once (`wrangler d1 execute usnm-check --remote --file migrations/0003-answer-font.sql` or the D1 query API).
ALTER TABLE answers ADD COLUMN font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown'));
ALTER TABLE answers_excluded ADD COLUMN font TEXT NOT NULL DEFAULT 'unknown' CHECK (font IN ('loaded', 'not_loaded', 'none', 'unknown'));
