/**
 * Free-text search over every processed day.
 *
 * The query is debounced rather than sent on each keystroke: the index is in memory
 * on the Rust side and answers quickly, but a request per character would still
 * queue results that are already stale by the time they arrive.
 */
import { useEffect, useState } from "react";
import { searchHistory, type SearchHit } from "../lib/ipc";
import { clockTime, duration, localHour } from "../lib/format";

const DEBOUNCE_MS = 300;
const LIMIT = 50;

interface Props {
  onOpenDay: (date: string, hour?: number) => void;
}

export default function Search({ onOpenDay }: Props) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [searched, setSearched] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const term = query.trim();
    if (term === "") {
      setHits([]);
      setSearched(false);
      return;
    }

    let current = true;
    const timer = setTimeout(() => {
      searchHistory(term, LIMIT)
        .then((found) => {
          if (!current) return;
          setHits(found);
          setSearched(true);
          setError(null);
        })
        .catch((cause) => current && setError(String(cause)));
    }, DEBOUNCE_MS);

    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [query]);

  return (
    <section aria-label="Search">
      <h2 className="section__title">Search</h2>

      <input
        type="search"
        className="input input--search"
        placeholder="Application, window title, or address"
        aria-label="Search history"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
      />

      {error ? (
        <p className="notice notice--error" role="alert">
          {error}
        </p>
      ) : null}

      {searched && hits.length === 0 ? (
        <p className="empty">Nothing matched every word of that.</p>
      ) : null}

      {hits.length > 0 ? (
        <>
          <p className="summary">
            {hits.length === LIMIT
              ? `First ${LIMIT} matches`
              : `${hits.length} match${hits.length === 1 ? "" : "es"}`}
          </p>
          <ol className="hits">
            {hits.map((hit) => (
              <li key={hit.id} className={`hit${hit.isPrivate ? " hit--private" : ""}`}>
                <button
                  type="button"
                  className="hit__open"
                  onClick={() => onOpenDay(hit.date, localHour(hit.start))}
                >
                  <span className="hit__app">{hit.app}</span>
                  <span className="hit__title">
                    {hit.isPrivate ? "Private browsing — title not recorded" : (hit.title ?? "")}
                  </span>
                  <span className="hit__when">
                    {hit.date} · <time dateTime={hit.start}>{clockTime(hit.start)}</time> ·{" "}
                    {duration(hit.activeMs)}
                  </span>
                </button>
              </li>
            ))}
          </ol>
        </>
      ) : null}
    </section>
  );
}
