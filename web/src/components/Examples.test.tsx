import { afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Examples } from "./Examples";
import { EXAMPLE_ORDER, EXAMPLES, type Example } from "../examples";

/** The cards as App shows them, with the press count held in state. */
function Home({ onPick = () => undefined }: { onPick?: (ex: Example) => void }) {
  const [clicks, setClicks] = useState(0);
  return <Examples order={EXAMPLE_ORDER} clicks={clicks} onMore={() => setClicks((c) => c + 1)} onPick={onPick} />;
}

const titles = () =>
  Array.from(document.querySelectorAll("#examples button.example strong")).map((el) => el.textContent ?? "");
const more = () => screen.queryByRole("button", { name: /^Show \d+ more examples?$/ });

describe("Examples", () => {
  afterEach(cleanup);

  it("shows three, then adds ten at a time without repeats until all have shown", () => {
    render(<Home />);
    const first = titles();
    expect(first).toEqual(EXAMPLE_ORDER.slice(0, 3).map((e) => e.title));
    expect(more()?.textContent).toBe("Show 10 more examples");
    expect(more()?.getAttribute("aria-controls")).toBe("examples");

    fireEvent.click(more()!);
    expect(titles()).toHaveLength(13);
    expect(titles().slice(0, 3)).toEqual(first);

    let presses = 1;
    while (more()) {
      const before = titles();
      const label = more()!.textContent;
      fireEvent.click(more()!);
      presses++;
      const after = titles();
      expect(after.slice(0, before.length)).toEqual(before);
      expect(label).toBe(`Show ${after.length - before.length} more examples`);
    }
    expect(presses).toBe(10);
    const all = titles();
    expect(all).toHaveLength(EXAMPLES.length);
    expect(new Set(all).size).toBe(EXAMPLES.length);
    expect(new Set(all)).toEqual(new Set(EXAMPLES.map((e) => e.title)));
  });

  it("moves focus to the first card each press adds", () => {
    render(<Home />);
    fireEvent.click(more()!);
    expect(document.activeElement?.querySelector("strong")?.textContent).toBe(EXAMPLE_ORDER[3]!.title);
    fireEvent.click(more()!);
    expect(document.activeElement?.querySelector("strong")?.textContent).toBe(EXAMPLE_ORDER[13]!.title);
  });

  it("names a last press of one example in the singular", () => {
    const order = EXAMPLE_ORDER.slice(0, 14);
    render(<Examples order={order} clicks={0} onMore={() => undefined} onPick={() => undefined} />);
    expect(more()?.textContent).toBe("Show 10 more examples");
    cleanup();
    render(<Examples order={order} clicks={1} onMore={() => undefined} onPick={() => undefined} />);
    expect(more()?.textContent).toBe("Show 1 more example");
    cleanup();
    render(<Examples order={order} clicks={2} onMore={() => undefined} onPick={() => undefined} />);
    expect(more()).toBeNull();
    expect(titles()).toHaveLength(14);
  });

  it("runs an example when its card is pressed", () => {
    const onPick = vi.fn();
    render(<Home onPick={onPick} />);
    fireEvent.click(document.querySelectorAll("#examples button.example")[1]!);
    expect(onPick).toHaveBeenCalledWith(EXAMPLE_ORDER[1]);
  });
});
