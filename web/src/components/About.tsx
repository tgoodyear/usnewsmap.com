import { forwardRef } from "react";

const ext = { target: "_blank", rel: "noopener noreferrer" } as const;

/**
 * The About statement, in a modal dialog so opening it keeps the map and the
 * search as they are. Callers open it with `ref.current.showModal()`; Escape,
 * the Close button and a click on the backdrop close it.
 */
export const About = forwardRef<HTMLDialogElement>(function About(_, ref) {
  return (
    <dialog
      ref={ref}
      className="about"
      aria-labelledby="about-title"
      onClick={(e) => {
        // Backdrop clicks target the dialog, but so do clicks on its border and
        // scrollbar; only a click outside its box is on the backdrop.
        const dialog = e.currentTarget;
        if (e.target !== dialog) return;
        const r = dialog.getBoundingClientRect();
        const inside = e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
        if (!inside) dialog.close();
      }}
    >
      <div className="about__body">
        <h2 id="about-title">About US News Map</h2>
        <p>
          US News Map searches{" "}
          <a href="https://chroniclingamerica.loc.gov/" {...ext}>
            Chronicling America
          </a>
          , the{" "}
          <a href="https://www.loc.gov/ndnp/" {...ext}>
            NEH and Library of Congress
          </a>{" "}
          collection of digitized American newspapers, and maps every matching page by where and when
          it was printed. Play the timeline to watch a word or a story move across the country.
        </p>
        <p>
          The site was launched in 2016 by the{" "}
          <a href="https://ehistory.org/" {...ext}>
            eHistory.org
          </a>{" "}
          group at the University of Georgia (
          <a href="https://claudiosaunt.com/" {...ext}>
            Claudio Saunt
          </a>{" "}
          and{" "}
          <a href="https://history.uga.edu/directory/people/stephen-berry" {...ext}>
            Steve Berry
          </a>
          ) and the{" "}
          <a href="https://gtri.gatech.edu/" {...ext}>
            Georgia Tech Research Institute
          </a>{" "}
          (
          <a href="https://www.linkedin.com/in/goodyear/" {...ext}>
            Trevor Goodyear
          </a>
          ,{" "}
          <a href="https://www.linkedin.com/in/david-ediger-5a771b12" {...ext}>
            David Ediger
          </a>{" "}
          and{" "}
          <a href="https://www.linkedin.com/in/zachary-suffern-0966825a" {...ext}>
            Zach Suffern
          </a>
          ). It was featured in{" "}
          <a
            href="https://web.archive.org/web/20190307100233/http://www.slate.com/blogs/the_vault/2016/03/07/us_news_map_interactive_lets_you_map_how_historical_newspapers_digitized.html"
            {...ext}
          >
            Slate
          </a>{" "}
          and{" "}
          <a
            href="https://web.archive.org/web/20160616154229/https://www.washingtonpost.com/news/the-intersect/wp/2016/03/17/the-secret-pre-internet-history-of-viral-memes/"
            {...ext}
          >
            The Washington Post
          </a>{" "}
          and won a prize in the{" "}
          <a href="https://web.archive.org/web/20170126055934/https://www.neh.gov/news/press-release/2016-07-25" {...ext}>
            Chronicling America Data Challenge
          </a>
          , run by the{" "}
          <a href="https://www.loc.gov/ndnp/" {...ext}>
            NEH and Library of Congress
          </a>
          . Georgia Tech's{" "}
          <a
            href="https://web.archive.org/web/20161228012501/http://www.news.gatech.edu/2016/03/06/what-going-viral-looked-120-years-ago/"
            {...ext}
          >
            2016 article
          </a>{" "}
          and{" "}
          <a href="https://www.youtube.com/watch?v=vrL-eZWNRiM" {...ext}>
            video
          </a>{" "}
          show it tracing William Jennings Bryan's "Cross of Gold" from Chicago to both coasts.
        </p>
        <p>
          The newspaper text comes from optical character recognition, so scanning errors mean some
          pages are missed and some matches are wrong. Each result links to the full page image at the
          Library of Congress.
        </p>
        <p>
          US News Map is maintained by{" "}
          <a href="https://goodyeartechnical.com/" {...ext}>
            Trevor Goodyear
          </a>
          .
        </p>
        <form method="dialog">
          <button type="submit" className="button">
            Close
          </button>
        </form>
      </div>
    </dialog>
  );
});
