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
    fireEvent.blur(text);
    fireEvent.mouseEnter(text);
    expect(note.hidden).toBe(false);
    fireEvent.mouseLeave(text);
    expect(note.hidden).toBe(true);
  });

  it("follows its text when the page scrolls or the window changes size, inside the window", () => {
    render(
      <p>
        <Tooltip note="A note.">
          <strong>395</strong> pages
        </Tooltip>
      </p>,
    );
    const note = screen.getByRole("tooltip", { hidden: true });
    const text = note.parentElement!;
    let rect = { left: 100, bottom: 50 };
    text.getBoundingClientRect = () => ({
      ...rect,
      top: rect.bottom - 20,
      right: rect.left + 80,
      width: 80,
      height: 20,
      x: rect.left,
      y: rect.bottom - 20,
      toJSON: () => ({}),
    });
    window.innerWidth = 1024;
    fireEvent.focus(text);
    expect([note.style.left, note.style.top]).toEqual(["100px", "56px"]);
    // Scrolled: lower down and further right.
    rect = { left: 300, bottom: 200 };
    fireEvent.scroll(window);
    expect([note.style.left, note.style.top]).toEqual(["300px", "206px"]);
    // A narrow window: the note stays inside it (320 px wide, 8 px from the edge).
    window.innerWidth = 400;
    fireEvent(window, new Event("resize"));
    expect([note.style.left, note.style.top]).toEqual(["72px", "206px"]);
  });

  it("stays while the pointer or the focus is still on its text", () => {
    render(
      <p>
        <Tooltip note="A note.">
          <strong>395</strong> pages
        </Tooltip>
      </p>,
    );
    const note = screen.getByRole("tooltip", { hidden: true });
    const text = note.parentElement!;
    // Focused, then hovered: leaving with the pointer keeps it for the focus.
    fireEvent.focus(text);
    fireEvent.mouseEnter(text);
    fireEvent.mouseLeave(text);
    expect(note.hidden).toBe(false);
    fireEvent.blur(text);
    expect(note.hidden).toBe(true);
    // Hovered, then focused: losing the focus keeps it for the pointer.
    fireEvent.mouseEnter(text);
    fireEvent.focus(text);
    fireEvent.blur(text);
    expect(note.hidden).toBe(false);
    fireEvent.mouseLeave(text);
    expect(note.hidden).toBe(true);
    // Scrolling and resizing move it, never hide it.
    fireEvent.focus(text);
    fireEvent.scroll(window);
    fireEvent(window, new Event("resize"));
    expect(note.hidden).toBe(false);
    // Escape hides it while both hold, until the pointer or the focus comes back.
    fireEvent.mouseEnter(text);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(note.hidden).toBe(true);
    fireEvent.mouseLeave(text);
    expect(note.hidden).toBe(true);
    fireEvent.mouseEnter(text);
    expect(note.hidden).toBe(false);
  });
});
