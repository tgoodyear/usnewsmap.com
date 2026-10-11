import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MeasureToggle } from "./MeasureToggle";
import { baselineNote } from "../lib/baseline";

afterEach(cleanup);

describe("the relative rate's explanation", () => {
  it("says an English search is compared with English-language newspapers' pages", () => {
    render(
      <MeasureToggle
        norm="skew"
        onChange={() => {}}
        baseline={baselineNote({ languages: ["eng"], why: "query_language" })}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "About the relative rate" }));
    expect(screen.getByText(/compared with the pages of its English-language newspapers/)).toBeTruthy();
    expect(screen.getByText(/mostly bilingual papers, still count/)).toBeTruthy();
  });

  it("says nothing about the pages when the API doesn't", () => {
    render(<MeasureToggle norm="skew" onChange={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "About the relative rate" }));
    expect(screen.getByText(/pulled toward the typical rate/)).toBeTruthy();
    expect(screen.queryByText(/compared with the pages of/)).toBeNull();
    expect(screen.queryByText(/in every language/)).toBeNull();
  });
});
