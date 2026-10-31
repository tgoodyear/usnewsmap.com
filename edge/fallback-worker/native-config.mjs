import { randomUUID } from "node:crypto";
import { readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parseJsonc } from "./parse-jsonc.mjs";

const source = resolve(process.argv[2]);
const config = parseJsonc(readFileSync(source, "utf8"));
config.main = resolve(dirname(source), config.main);
config.cache = { enabled: false };
const native = process.argv[3] !== "custom-domain";
const hostname = process.argv[4];
const workerLogs = process.argv[5] === "worker-logs";
if (native) {
  config.analytics_engine_datasets = [];
  config.vars = { ...config.vars, METRICS_MODE: "native" };
}
if (hostname) {
  if (hostname !== "healthcheck.usnewsmap.com") throw new Error("Invalid healthcheck hostname");
  config.routes = [...config.routes, { pattern: hostname, custom_domain: true }];
  config.vars = { ...config.vars, HEALTHCHECK_HOSTNAME: hostname };
}
if (workerLogs) {
  config.vars = { ...config.vars, LOG_OUTCOMES: "true" };
  config.observability = {
    enabled: true,
    logs: { enabled: true, invocation_logs: true },
    traces: { enabled: false },
    redact_query_string: true,
  };
}
const target = resolve(dirname(source), `.native-metrics-${randomUUID()}.json`);
try {
  writeFileSync(target, `${JSON.stringify(config, null, 2)}\n`, { flag: "wx", mode: 0o600 });
  process.stdout.write(`${target}\n`);
} catch (error) {
  if (error.code !== "EEXIST") {
    try { unlinkSync(target); } catch {}
  }
  throw error;
}
