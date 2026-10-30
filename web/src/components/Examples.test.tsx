import { afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Examples } from "./Examples";
import { EXAMPLE_ORDER, EXAMPLES, EXAMPLES_MORE, EXAMPLES_SHOWN } from "../examples";
import { DEFAULTS, parseViewUrl, viewUrl, type ViewState } from "../state/url";
import { searchPath } from "../route";

/** The cards as App shows them, with the press count held in state. */
function Home({ onPick = () => undefined }: { onPick?: (view: ViewState) => void }) {
  const [clicks, setClicks] = useState(0);
  return <Examples order={EXAMPLE_ORDER} clicks={clicks} onMore={() => setClicks((c) => c + 1)} onPick={onPick} />;
}

const titles = () =>
  Array.from(document.querySelectorAll("#examples a.example strong")).map((el) => el.textContent ?? "");
const more = () => screen.queryByRole("button", { name: "Show more examples" });

describe("Examples", () => {
  afterEach(cleanup);

  it("shows three, then adds ten at a time without repeats until all have shown", () => {
    render(<Home />);
    const first = titles();
    expect(first).toEqual(EXAMPLE_ORDER.slice(0, 3).map((e) => e.title));
    expect(more()?.textContent).toBe("Show more examples");
    expect(more()?.getAttribute("aria-controls")).toBe("examples");

    fireEvent.click(more()!);
    expect(titles()).toHaveLength(13);
    expect(titles().slice(0, 3)).toEqual(first);

    let presses = 1;
    while (more()) {
      const before = titles();
      fireEvent.click(more()!);
      presses++;
      const after = titles();
      expect(after.slice(0, before.length)).toEqual(before);
      expect(after.length - before.length).toBe(Math.min(10, EXAMPLES.length - before.length));
    }
    expect(presses).toBe(Math.ceil((EXAMPLES.length - EXAMPLES_SHOWN) / EXAMPLES_MORE));
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

  it("keeps the same label for a last, short press", () => {
    const order = EXAMPLE_ORDER.slice(0, 14);
    render(<Examples order={order} clicks={0} onMore={() => undefined} onPick={() => undefined} />);
    expect(more()?.textContent).toBe("Show more examples");
    cleanup();
    render(<Examples order={order} clicks={1} onMore={() => undefined} onPick={() => undefined} />);
    expect(more()?.textContent).toBe("Show more examples");
    cleanup();
    render(<Examples order={order} clicks={2} onMore={() => undefined} onPick={() => undefined} />);
    expect(more()).toBeNull();
    expect(titles()).toHaveLength(14);
  });

  it("runs an example when its card is pressed", () => {
    const onPick = vi.fn();
    render(<Home onPick={onPick} />);
    const link = document.querySelectorAll<HTMLAnchorElement>("#examples a.example")[1]!;
    expect(link.getAttribute("href")).toMatch(/^\/search\//);
    fireEvent.click(link);
    expect(onPick).toHaveBeenCalledWith({ ...DEFAULTS, ...EXAMPLE_ORDER[1]!.view });
    expect(viewUrl(onPick.mock.calls[0]![0])).toBe(link.getAttribute("href"));
  });

  it("filters by canonical base query without losing historical filters or promoted links", () => {
    const example = EXAMPLE_ORDER[0]!;
    const query = example.view.q ?? DEFAULTS.q;
    const base = searchPath(query);
    const onPick = vi.fn();
    render(
      <Examples
        order={EXAMPLE_ORDER}
        clicks={100}
        onMore={() => undefined}
        onPick={onPick}
        searchLinks={[
          { href: base, label: query },
          { href: "/search/cobalt-jubilee", label: "cobalt jubilee" },
        ]}
      />,
    );
    const card = screen.getByRole("link", { name: `${example.title}${example.blurb}` });
    expect(card.getAttribute("href")).toBe(viewUrl({ ...DEFAULTS, ...example.view }));
    fireEvent.click(card);
    expect(onPick).toHaveBeenCalledWith({ ...DEFAULTS, ...example.view });
    expect(screen.queryByRole("link", { name: new RegExp(EXAMPLE_ORDER[1]!.title) })).toBeNull();
    expect(screen.getByRole("link", { name: /cobalt jubilee/ }).getAttribute("href")).toBe("/search/cobalt-jubilee");
  });

  it("distinguishes verified empty eligibility from unavailable eligibility", () => {
    const props = { order: EXAMPLE_ORDER, clicks: 0, onMore: () => undefined, onPick: () => undefined };
    const { rerender } = render(<Examples {...props} searchLinks={[]} />);
    expect(titles()).toEqual([]);
    expect(more()).toBeNull();
    rerender(<Examples {...props} />);
    expect(titles()).toEqual(EXAMPLE_ORDER.slice(0, 3).map((ex) => ex.title));
  });

  it.each([
    { label: "Relative rate", norm: "skew", layer: "points" },
    { label: "States", norm: "raw", layer: "states" },
  ])("preserves $label and historical filters in both the card click and permalink", ({ norm, layer }) => {
    const example = EXAMPLES.find(
      (ex) => (ex.view.norm ?? DEFAULTS.norm) === norm && (ex.view.layer ?? DEFAULTS.layer) === layer,
    );
    expect(example).toBeDefined();
    const onPick = vi.fn();
    render(<Examples order={[example!]} clicks={0} onMore={() => undefined} onPick={onPick} />);
    const link = screen.getByRole("link");
    const expected = { ...DEFAULTS, ...example!.view };
    expect(link.getAttribute("href")).toBe(viewUrl(expected));
    const linked = parseViewUrl(link.getAttribute("href")!);
    expect({ ...linked, q: expected.q }).toEqual(expected);
    fireEvent.click(link);
    expect(onPick).toHaveBeenCalledWith(expected);
  });
});
