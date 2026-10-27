import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import PrivacyPage from "./PrivacyPage";

describe("PrivacyPage", () => {
  afterEach(cleanup);

  it("has one plain heading, a date, the contact page and no email address", () => {
    const { container } = render(<PrivacyPage />);
    expect(screen.getAllByRole("heading", { level: 1 }).map((h) => h.textContent)).toEqual(["Privacy"]);
    expect(document.title).toBe("Privacy · US News Map");
    expect(container.textContent).toMatch(/Last updated \d{1,2} \w+ \d{4}\./);
    expect(screen.getByRole("link", { name: /goodyeartechnical\.com\/contact/ })).toHaveProperty(
      "href",
      "https://goodyeartechnical.com/contact/",
    );
    expect(container.textContent).not.toMatch(/\S+@\S+\.\w+/);
    expect(container.textContent).not.toContain("—");
  });

  it("says search words are kept indefinitely, without identifiers, and not under DNT or GPC", () => {
    const { container } = render(<PrivacyPage />);
    const text = container.textContent ?? "";
    expect(text).toContain("keeps the words of every search indefinitely");
    expect(text).toContain("never stored with your IP address, browser details, location or any identifier");
    expect(text).toContain("Do Not Track or Global Privacy Control, your searches are not recorded");
    expect(text).toContain("at least five times");
    expect(text).not.toContain("copied from the search engine's own log");
  });

  it("discloses Cloudflare processing while retaining Azure serving and search bypass", () => {
    const { container } = render(<PrivacyPage />);
    const text = container.textContent ?? "";
    expect(screen.getByRole("heading", { name: "Cloudflare" })).toBeTruthy();
    expect(text).toContain("before reaching our servers in Microsoft Azure");
    expect(text).toContain("for up to 31 days");
    expect(text).toContain("but not search results");
    expect(text).toContain("Do Not Track and Global Privacy Control do not prevent this processing");
    expect(screen.getByRole("link", { name: "Cloudflare privacy policy" }).getAttribute("href")).toBe(
      "https://www.cloudflare.com/privacypolicy/",
    );
  });
});
