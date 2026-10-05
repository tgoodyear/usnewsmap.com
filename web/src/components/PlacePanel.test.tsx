import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import type { HitItem } from "../api/types";
import { Hit } from "./PlacePanel";

const page: HitItem = {
  doc_id: "sn83025517_1945-01-01_ed-1_seq-4",
  date: "1945-01-01",
  lccn: "sn83025517",
  title: "Rocky Shimpo",
  place_id: "P00001",
  edition: 1,
  seq: 4,
  front_page: false,
  snippets: ["去年の<mark>大</mark>記事"],
  links: { viewer: "https://www.loc.gov/resource/sn83025517/1945-01-01/ed-1/?sp=4" },
};

afterEach(cleanup);

describe("Hit", () => {
  it("marks a page whose text is our own OCR and links to LoC's page image", () => {
    render(<ul><Hit h={{ ...page, ocr: { source: "usnm-ndlocr-lite", engine: "ndlocr-lite 636d1cf" } }} synthetic={false} /></ul>);
    const badge = screen.getByText(/^Our OCR/);
    expect(badge.getAttribute("title")).toMatch(/no searchable text for this page/);
    // The explanation is in the text too, for screen readers and touch screens.
    expect(badge.textContent).toMatch(/^Our OCR\. The Library of Congress has no searchable text/);
    expect(screen.getByRole("link").textContent).toBe("View the page image at the Library of Congress");
  });

  it("shows a LoC page as before", () => {
    render(<ul><Hit h={page} synthetic={false} /></ul>);
    expect(screen.queryByText(/^Our OCR/)).toBeNull();
    expect(screen.getByRole("link").textContent).toBe("View page at the Library of Congress");
  });
});
