/** Whether the map can render; without WebGL2 the table view is shown. */
export function hasWebGL2(): boolean {
  try {
    return !!document.createElement("canvas").getContext("webgl2");
  } catch {
    return false;
  }
}
