// Fits the relative-rate model off the main thread: the fit takes tens of
// milliseconds for a whole-corpus monthly search on a laptop, more on a phone.

import { buildSkew, type SkewInput } from "./skewModel";

self.onmessage = (e: MessageEvent<{ id: number; input: SkewInput }>) => {
  const { id, input } = e.data;
  try {
    const model = buildSkew(input);
    const buffers = [model.places, model.states].flatMap((s) => [s.observed.buffer, s.expected.buffer, s.pages.buffer]);
    (self as unknown as Worker).postMessage({ id, model }, buffers);
  } catch (err) {
    (self as unknown as Worker).postMessage({ id, error: String(err) });
  }
};
