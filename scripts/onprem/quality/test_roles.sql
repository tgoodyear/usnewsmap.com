-- Checks setup.sql's roles (#962): each may do what its row in README.md says and nothing
-- else. Run as postgres in usnm_quality (ci-fixture.sh does); fails on the first mismatch.
DO $$
DECLARE
  want record;
  got boolean;
BEGIN
  FOR want IN
    SELECT * FROM (VALUES
      -- role, table, privilege, expected
      ('quality_loader', 'quality.pages', 'INSERT', true),
      ('quality_loader', 'quality.pages', 'UPDATE', true),
      ('quality_loader', 'quality.damage', 'INSERT', true),
      ('quality_loader', 'quality.unit_pages', 'DELETE', true),
      ('quality_loader', 'quality.reader_scores', 'UPDATE', true),
      ('quality_loader', 'quality.staging', 'INSERT', true),
      ('quality_loader', 'quality.readers', 'INSERT', true),
      ('quality_loader', 'quality.readers', 'UPDATE', false),
      ('quality_loader', 'quality.text_compare', 'INSERT', false),
      ('quality_loader', 'quality.page_summary', 'SELECT', true),
      ('quality_text_writer', 'quality.text_compare', 'INSERT', true),
      ('quality_text_writer', 'quality.text_compare', 'UPDATE', true),
      ('quality_text_writer', 'quality.text_compare', 'DELETE', false),
      ('quality_text_writer', 'quality.campaigns', 'INSERT', true),
      ('quality_text_writer', 'quality.campaigns', 'UPDATE', false),
      ('quality_text_writer', 'quality.pages', 'SELECT', true),
      ('quality_text_writer', 'quality.pages', 'INSERT', false),
      ('quality_text_writer', 'quality.damage', 'UPDATE', false),
      ('quality_text_writer', 'quality.unit_pages', 'INSERT', false),
      ('quality_reader', 'quality.page_summary', 'SELECT', true),
      ('quality_reader', 'quality.pages', 'SELECT', true),
      ('quality_reader', 'quality.pages', 'INSERT', false),
      ('quality_reader', 'quality.text_compare', 'INSERT', false),
      ('quality_reader', 'quality.campaigns', 'INSERT', false),
      ('quality_reader', 'quality.reader_scores', 'UPDATE', false)
    ) AS t(role, tbl, priv, expected)
  LOOP
    got := has_table_privilege(want.role, want.tbl, want.priv);
    IF got IS DISTINCT FROM want.expected THEN
      RAISE EXCEPTION '% % on %: % (expected %)', want.role, want.priv, want.tbl, got, want.expected;
    END IF;
  END LOOP;
  IF NOT has_function_privilege('quality_loader', 'quality.refresh_summary()', 'EXECUTE') THEN
    RAISE EXCEPTION 'quality_loader can''t refresh page_summary';
  END IF;
  IF has_function_privilege('quality_reader', 'quality.refresh_summary()', 'EXECUTE')
     OR has_function_privilege('quality_text_writer', 'quality.refresh_summary()', 'EXECUTE') THEN
    RAISE EXCEPTION 'only quality_loader may refresh page_summary';
  END IF;
  IF has_database_privilege('quality_reader', 'usnm_quality', 'TEMPORARY') THEN
    RAISE EXCEPTION 'quality_reader may create temporary tables';
  END IF;
  IF has_database_privilege('quality_loader', 'usnm_quality', 'CREATE') THEN
    RAISE EXCEPTION 'quality_loader may create schemas in usnm_quality';
  END IF;
  IF has_schema_privilege('quality_loader', 'quality', 'CREATE') THEN
    RAISE EXCEPTION 'quality_loader may create objects in schema quality';
  END IF;
END $$;

-- And through the function: the loader's refresh runs.
SET ROLE quality_loader;
SELECT quality.refresh_summary();
RESET ROLE;
SELECT 'roles checked' AS result;
