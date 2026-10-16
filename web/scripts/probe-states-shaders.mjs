import { chromium } from "@playwright/test";
import { cpus, platform, arch } from "node:os";

const args = process.argv.slice(2);
const baseIndex = args.indexOf("--base-url");
if (baseIndex < 0 || !args[baseIndex + 1]) {
  throw new Error(
    "Usage: node scripts/probe-states-shaders.mjs --base-url http://127.0.0.1:18092 [--metal] [--parallel]",
  );
}
const launchOptions = {
  args: [
    ...(args.includes("--metal") ? ["--use-angle=metal"] : []),
    "--disable-gpu-shader-disk-cache",
    "--disable-gpu-program-cache",
  ],
};
const capture = await chromium.launch(launchOptions);
let programs;
try {
  const page = await capture.newPage();
  await page.addInitScript(() => {
    const sourceByShader = new WeakMap();
    const shadersByProgram = new WeakMap();
    globalThis.stateShaderPrograms = [];
    const prototype = WebGL2RenderingContext.prototype;
    const source = prototype.shaderSource;
    const attach = prototype.attachShader;
    const parameter = prototype.getProgramParameter;
    prototype.shaderSource = function (shader, text) {
      sourceByShader.set(shader, text);
      return source.call(this, shader, text);
    };
    prototype.attachShader = function (program, shader) {
      const shaders = shadersByProgram.get(program) ?? [];
      shaders.push(shader);
      shadersByProgram.set(program, shaders);
      return attach.call(this, program, shader);
    };
    prototype.getProgramParameter = function (program, name) {
      if (name === this.LINK_STATUS) {
        const sources = (shadersByProgram.get(program) ?? []).map((shader) => ({
          type: this.getShaderParameter(shader, this.SHADER_TYPE),
          source: sourceByShader.get(shader),
        }));
        if (globalThis.captureStateShaders) {
          globalThis.stateShaderPrograms.push(sources);
        }
      }
      return parameter.call(this, program, name);
    };
  });
  await page.route("https://tiles.openfreemap.org/**", (route) =>
    route.fulfill({ json: { version: 8, sources: {}, layers: [] } }),
  );
  await page.goto(new URL("/?q=%22cross+of+gold%22&bucket=month", args[baseIndex + 1]).href);
  await page.getByRole("combobox", { name: "Map layer" }).waitFor();
  await page.waitForTimeout(500);
  await page.evaluate(() => {
    globalThis.captureStateShaders = true;
  });
  await page.getByRole("combobox", { name: "Map layer" }).selectOption("states");
  await page.waitForFunction(() => performance.getEntriesByName("usnm-states-first-paint").length > 0);
  programs = await page.evaluate(() => globalThis.stateShaderPrograms);
} finally {
  await capture.close();
}

console.log(
  JSON.stringify({ host: { cpu: cpus()[0]?.model, platform: platform(), arch: arch() }, programs: programs.length }),
);
for (let sample = 0; sample < 5; sample++) {
  const browser = await chromium.launch(launchOptions);
  try {
    const page = await browser.newPage();
    const result = await page.evaluate(
      async ({ programs, offset, parallel }) => {
        const gl = document.createElement("canvas").getContext("webgl2");
        if (!gl) throw new Error("WebGL2 unavailable");
        const debug = gl.getExtension("WEBGL_debug_renderer_info");
        const extension = gl.getExtension("KHR_parallel_shader_compile");
        const startAll = performance.now();
        const pending = [];
        const timings = [];
        const completeProgram = (entry) => {
          const start = performance.now();
          const linked = gl.getProgramParameter(entry.program, gl.LINK_STATUS);
          const complete = performance.now();
          if (!linked) throw new Error(gl.getProgramInfoLog(entry.program));
          gl.deleteProgram(entry.program);
          timings.push({ enqueueMs: entry.enqueueMs, linkStatusBlockMs: complete - start });
        };
        for (const sources of programs) {
          const program = gl.createProgram();
          const start = performance.now();
          for (const original of sources) {
            const shader = gl.createShader(original.type);
            let text = original.source;
            if (original.type === gl.VERTEX_SHADER) {
              text = text.replace(/void main\s*\(\s*(?:void)?\s*\)\s*\{/, "void measuredOriginalMain() {");
              text += `\nvoid main() { measuredOriginalMain(); gl_Position.x += ${offset} * gl_Position.w; }\n`;
            }
            gl.shaderSource(shader, text);
            gl.compileShader(shader);
            gl.attachShader(program, shader);
            gl.deleteShader(shader);
          }
          gl.linkProgram(program);
          const queued = performance.now();
          const entry = { program, enqueueMs: queued - start };
          if (parallel) pending.push(entry);
          else completeProgram(entry);
        }
        if (parallel) {
          if (!extension) throw new Error("KHR_parallel_shader_compile unavailable");
          while (pending.some(({ program }) => !gl.getProgramParameter(program, extension.COMPLETION_STATUS_KHR))) {
            await new Promise((resolve) => setTimeout(resolve, 1));
          }
          pending.forEach(completeProgram);
        }
        return {
          renderer: debug ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL) : "unknown",
          parallelCompile: !!extension,
          mode: parallel ? "enqueue all, poll completion, then inspect" : "serial immediate LINK_STATUS",
          timings,
          totalMs: performance.now() - startAll,
        };
      },
      { programs, offset: (1e-8 + Math.random() * 1e-8).toPrecision(9), parallel: args.includes("--parallel") },
    );
    console.log(JSON.stringify({ sample, browser: browser.version(), ...result }));
  } finally {
    await browser.close();
  }
}
