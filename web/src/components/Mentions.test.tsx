import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { HitItem } from "../api/types";
import { Mentions } from "./Mentions";

const hit = (doc_id: string, date: string, place_id: string, title: string | null): HitItem => ({
  doc_id,
  date,
  lccn: "sn99000001",
  title,
  place_id,
  edition: 1,
  seq: 1,
  front_page: true,
  snippets: [],
  links: { viewer: null },
});

const first = hit("a", "1896-07-10", "P00001", "The Chicago Eagle");
const last = hit("b", "1896-12-28", "P00006", null);
const names: Record<string, string> = { P00001: "Chicago, IL", P00006: "Omaha, NE" };

describe("Mentions", () => {
  afterEach(cleanup);

  it("names the first page and links both dates to their place's list", () => {
    const onOpen = vi.fn();
    render(
      <Mentions
        first={first}
        last={last}
        placeName={(id) => names[id] ?? id}
        hrefFor={(h, sort) => `/?place=${h.place_id}&sort=${sort}`}
        onOpen={onOpen}
      />,
    );
    expect(screen.getByText(/First mention/).textContent).toBe(
      "First mention: Jul 10, 1896, The Chicago Eagle, Chicago, IL · Last: Dec 28, 1896",
    );
    const firstLink = screen.getByRole("link", { name: "Jul 10, 1896" });
    const lastLink = screen.getByRole("link", { name: "Dec 28, 1896" });
    expect(firstLink.getAttribute("href")).toBe("/?place=P00001&sort=oldest");
    expect(lastLink.getAttribute("href")).toBe("/?place=P00006&sort=newest");

    fireEvent.click(lastLink);
    expect(onOpen).toHaveBeenCalledWith(last, "newest");
    // A modified click opens the permalink instead.
    fireEvent.click(firstLink, { ctrlKey: true });
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("leaves out the last mention when it is the same page", () => {
    render(<Mentions first={first} last={first} placeName={(id) => id} hrefFor={() => "/"} onOpen={() => undefined} />);
    expect(screen.queryByText(/Last:/)).toBeNull();
    expect(screen.getAllByRole("link")).toHaveLength(1);
  });
});
