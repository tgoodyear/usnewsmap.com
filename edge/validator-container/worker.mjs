// The usnm validator for the cloud BOINC server, as a Cloudflare Container (README.md). The
// pipeline's webhook calls this Worker; it checks the bearer token and hands the call to one of a
// few container instances running `usnm-boinc cloud validatord`, which keeps its state in R2.
import { Container, getRandom } from "@cloudflare/containers";
import { INSTANCES, containerEnv, route } from "./route.mjs";

export class Validator extends Container {
  defaultPort = 8787;
  // A webhook call comes at least every few minutes while work is flowing; an idle instance stops.
  sleepAfter = "15m";

  constructor(ctx, env) {
    super(ctx, env);
    this.envVars = containerEnv(env);
  }
}

export default {
  async fetch(request, env) {
    const decision = route(request, env);
    if (!decision.forward) {
      return new Response(decision.body, {
        status: decision.status,
        headers: { "Content-Type": "text/plain; charset=utf-8" },
      });
    }
    const instance = await getRandom(env.VALIDATOR, INSTANCES);
    return instance.fetch(request);
  },
};
