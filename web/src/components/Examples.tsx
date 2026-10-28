import { useEffect, useRef } from "react";
import { examplesShown, type Example } from "../examples";
import { DEFAULTS, viewUrl, type ViewState } from "../state/url";

interface Props {
  /** Every example, in the order to show them. */
  order: readonly Example[];
  /** How many times "Show more" has been pressed. */
  clicks: number;
  onMore: () => void;
  onPick: (view: ViewState) => void;
}

/**
 * The home page's example cards: the first few of `order`, and a button that
 * adds the next ones below them until every example has shown.
 */
export function Examples({ order, clicks, onMore, onPick }: Props) {
  const shown = examplesShown(order, clicks);
  const left = order.length - shown.length;
  const list = useRef<HTMLUListElement>(null);
  // After "Show more", the index of the first card it added, to move focus
  // there: keyboard and screen reader users land on the new cards, and focus
  // isn't lost when the button goes away after the last ones.
  const focusFrom = useRef<number | null>(null);

  useEffect(() => {
    const index = focusFrom.current;
    if (index === null) return;
    focusFrom.current = null;
    list.current?.children[index]?.querySelector("a")?.focus();
  }, [shown.length]);

  return (
    <>
      <ul className="examples" id="examples" ref={list}>
        {shown.map((ex) => {
          const view = { ...DEFAULTS, ...ex.view };
          return (
            <li key={ex.id}>
              <a
                href={viewUrl(view)}
                className="example"
                onClick={(event) => {
                  if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
                  event.preventDefault();
                  onPick(view);
                }}
              >
                <strong>{ex.title}</strong>
                <span>{ex.blurb}</span>
              </a>
            </li>
          );
        })}
      </ul>
      {left > 0 && (
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
            Show more examples
          </button>
        </p>
      )}
    </>
  );
}
