import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Tooltip } from "./Tooltip";

afterEach(cleanup);

describe("Tooltip", () => {
  it("describes its text, takes keyboard focus and shows the note while focused", () => {
    render(
      <p>
        <Tooltip note="16 of the 395 pages match only in American Stories' text.">
          <strong>395</strong> pages
        </Tooltip>
      </p>,
    );
    const note = screen.getByRole("tooltip", { hidden: true });
    const text = note.parentElement!;
    expect(text.tabIndex).toBe(0);
    expect(text.getAttribute("aria-describedby")).toBe(note.id);
    expect(note.hidden).toBe(true);
    fireEvent.focus(text);
    expect(note.hidden).toBe(false);
    expect(note.textContent).toBe("16 of the 395 pages match only in American Stories' text.");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(note.hidden).toBe(true);
    fireEvent.mouseEnter(text);
    expect(note.hidden).toBe(false);
    fireEvent.mouseLeave(text);
    expect(note.hidden).toBe(true);
  });
});
