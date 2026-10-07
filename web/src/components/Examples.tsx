import { useEffect, useRef } from "react";
import { EXAMPLES_MORE, examplesShown, type Example } from "../examples";

interface Props {
  /** Every example, in the order to show them. */
  order: readonly Example[];
  /** How many times "Show more" has been pressed. */
  clicks: number;
  onMore: () => void;
  onPick: (example: Example) => void;
}

/**
 * The home page's example cards: the first few of `order`, and a button that
 * adds the next ones below them until every example has shown.
 */
export function Examples({ order, clicks, onMore, onPick }: Props) {
  const shown = examplesShown(order, clicks);
  const next = Math.min(EXAMPLES_MORE, order.length - shown.length);
  const list = useRef<HTMLUListElement>(null);
  // After "Show more", the index of the first card it added, to move focus
  // there: keyboard and screen reader users land on the new cards, and focus
  // isn't lost when the button goes away after the last ones.
  const focusFrom = useRef<number | null>(null);

  useEffect(() => {
    const index = focusFrom.current;
    if (index === null) return;
    focusFrom.current = null;
    list.current?.children[index]?.querySelector("button")?.focus();
  }, [shown.length]);

  return (
    <>
      <ul className="examples" id="examples" ref={list}>
        {shown.map((ex) => (
          <li key={ex.id}>
            <button type="button" className="example" onClick={() => onPick(ex)}>
              <strong>{ex.title}</strong>
              <span>{ex.blurb}</span>
            </button>
          </li>
        ))}
      </ul>
      {next > 0 && (
        <p className="examples__more">
          <button
            type="button"
            className="link-button"
            aria-controls="examples"
            onClick={() => {
              focusFrom.current = shown.length;
              onMore();
            }}
          >
            Show {next} more {next === 1 ? "example" : "examples"}
          </button>
        </p>
      )}
    </>
  );
}
