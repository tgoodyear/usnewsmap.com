import { useQuery } from "@tanstack/react-query";
import { count } from "./format";
import { Ext, Table, Time } from "./parts";

const PROJECT = "https://boinc.usnewsmap.com";
const LABELS: Record<string, string> = {
  usnm_ocr: "Page transcription",
  usnm_gold: "Accuracy checks",
  usnm_assess: "Scan-format comparisons",
  usnm_surya: "Surya OCR experiments",
  usnm_quality: "OCR quality comparisons",
  usnm_qwen: "Qwen transcription experiments",
  usnm_qwen_train: "Qwen training experiments",
  usnm_ndlocr: "Japanese OCR",
  usnm_iris: "Intel GPU OCR experiments",
};

export interface BoincSummary {
  schema: 1;
  updated_at: string;
  apps: { name: string; ready: number; in_progress: number; validated: number }[];
}

export function parseSummary(value: unknown): BoincSummary {
  if (
    !value ||
    typeof value !== "object" ||
    !("schema" in value) ||
    value.schema !== 1 ||
    !("updated_at" in value) ||
    typeof value.updated_at !== "string" ||
    !Number.isFinite(Date.parse(value.updated_at)) ||
    !("apps" in value) ||
    !Array.isArray(value.apps)
  ) {
    throw new Error("Invalid BOINC summary");
  }
  const names = new Set<string>();
  const apps = value.apps.map((app: unknown) => {
    if (
      !app ||
      typeof app !== "object" ||
      !("name" in app) ||
      typeof app.name !== "string" ||
      names.has(app.name) ||
      !("ready" in app) ||
      !("in_progress" in app) ||
      !("validated" in app)
    ) {
      throw new Error("Invalid BOINC task counts");
    }
    const { ready, in_progress, validated } = app;
    if (
      typeof ready !== "number" ||
      typeof in_progress !== "number" ||
      typeof validated !== "number" ||
      ![ready, in_progress, validated].every((n) => Number.isSafeInteger(n) && n >= 0)
    ) {
      throw new Error("Invalid BOINC task counts");
    }
    names.add(app.name);
    return { name: app.name, ready, in_progress, validated };
  });
  return { schema: 1, updated_at: value.updated_at, apps };
}

export function BoincSection({ now }: { now: number }) {
  const summary = useQuery({
    queryKey: ["boinc-summary"],
    queryFn: async ({ signal }) => {
      const response = await fetch(`${PROJECT}/work_summary.php`, {
        signal: AbortSignal.any([signal, AbortSignal.timeout(10_000)]),
        credentials: "omit",
      });
      if (!response.ok) throw new Error("BOINC summary unavailable");
      return parseSummary(await response.json());
    },
    refetchInterval: 120_000,
    retry: 1,
  });
  const data = summary.data;
  const stale = data && (summary.isError || now - Date.parse(data.updated_at) > 10 * 60_000);
  const active = data?.apps.filter((app) => app.ready + app.in_progress + app.validated > 0);
  const totals = data?.apps.reduce(
    (sum, app) => ({
      ready: sum.ready + app.ready,
      in_progress: sum.in_progress + app.in_progress,
      validated: sum.validated + app.validated,
    }),
    { ready: 0, in_progress: 0, validated: 0 },
  );
  return (
    <section aria-labelledby="boinc" className="status-section">
      <h2 id="boinc">Volunteer computing with BOINC</h2>
      <p>
        Volunteers lend CPU or GPU time to transcribe scans and test OCR methods. This is a pilot: available tasks
        depend on your computer and project preferences. <Ext href={`${PROJECT}/about.php`}>How to participate</Ext>.
      </p>
      {data ? (
        <>
          {stale && <p role="status">BOINC counts are out of date; showing the last report.</p>}
          {totals && (
            <p>
              <strong>{count(totals.ready)} ready to send</strong> ·{" "}
              <strong>{count(totals.in_progress)} in progress</strong> ·{" "}
              <strong>{count(totals.validated)} completed and validated</strong>
            </p>
          )}
          <Table
            caption="BOINC tasks by application"
            head={["Work", "Ready to send", "In progress", "Completed and validated"]}
            numeric={[1, 2, 3]}
            empty="No ready, in-progress or completed-and-validated tasks are currently recorded."
            rows={(active ?? []).map((app) => [
              LABELS[app.name] ?? app.name,
              count(app.ready),
              count(app.in_progress),
              count(app.validated),
            ])}
          />
          <p className="status-meta">
            Reported <Time iso={data.updated_at} now={now} />. Counts refresh every two minutes and cover tasks still
            retained in the BOINC database, not lifetime totals or the full unqueued backlog. Retries and replicas are
            separate tasks, not unique newspaper pages.
          </p>
        </>
      ) : (
        <p role="status">
          {summary.isError ? "BOINC task counts are temporarily unavailable." : "Loading BOINC task counts…"}
        </p>
      )}
      <p>
        Completed and validated means a task passed BOINC validation—not that its text is accepted or searchable.
        Quality checks, collection and an index update are separate steps.{" "}
        <Ext href={`${PROJECT}/server_status.php`}>Project status</Ext>.
      </p>
    </section>
  );
}
