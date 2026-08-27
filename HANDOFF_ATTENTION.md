# Handoff — attention and distraction

Written 2026-08-27 in the evening, before the work started, and rewritten at the end of
it on 2026-08-28. This covers one task only: making the summaries and the day chat able
to say something true about how scattered a stretch of work was. The project-wide
handoff is `HANDOFF.md` and is unchanged by this.

Read "The finding that shaped it" first. It is the reason the change is larger than a
prompt edit, and if it is ever undone the feature quietly stops working while still
appearing to.

## Where to pick up

**Five drafts are waiting in `drafts/luna-distraction/`.** Read that directory's
`README.md`, then the five numbered files, and say which framing you want. Draft 1 is
the control — exactly what the app sends today — so if it wins, nothing more is needed.
Otherwise tell me the number and I will make the app send that framing.

The drafts are **not committed**. They quote window titles, document names and screen
text from your own log, and this repository has a GitHub remote, so `/drafts` went into
`.gitignore` rather than to `origin`. Everything else here is pushed.

## What was asked

1. Change the system prompts wherever needed so the app can give an insight into how
   distracted you are while working, and whether there are frequent tab switches. Make
   inferences rather than restating totals.
2. Do not run the usual sweep. Build and push.
3. Write a separate, standalone test for this task. Use Luna — the configured cloud
   model, `gpt-5.6-luna` on the OpenAI Responses API.
4. Ask it: *how distracted was I while working on the critical in the evening today*,
   with today's real data.
5. Produce five drafts of the answer to choose between in the morning.

Points 2 to 5 were done without asking, as instructed. The one thing done beyond the
brief is explained under "The finding that shaped it": the prompt change alone would
not have worked.

## The finding that shaped it

"The critical" is `Vihaan_Pachisia_Critical_Essay_Draft_FEEDBACK` and `final crit`, two
Microsoft Word documents worked on that evening.

The evening of 2026-08-27 — every episode from 17:00 on — measures like this:

| band | visits | active | share |
|------|--------|--------|-------|
| under 10s | 233 | 10.6 min | 15.9% |
| 10s to 1 min | 80 | 30.2 min | 45.5% |
| 1 min and over | 12 | 25.6 min | 38.6% |

325 foreground visits, 66 minutes of active time, 323 switches.

`prompt.rs` had a constant `MIN_EPISODE_MS = 60_000`, and every prompt dropped any
episode below it, replacing the lot with a parenthesis: *(308 briefer switches, each
under a minute, are counted in the total above but are not listed: they were not worked
in.)* That line was added deliberately the day before, for a good reason — ten seconds
in Windows Terminal had been coming back as "five minutes of command-line activity".

It is wrong here, and wrong in a way that matters more than the bug it fixed.
**45% of the evening lived in the band it deleted.** Microsoft Word took 104 separate
visits totalling 14.5 minutes, median visit 4.2 seconds, and not one visit over a
minute. Of the 104 exits from Word, 95 went straight to Markdown Renderer, and 93 of
the 104 entries to Word came from it. Word and Markdown Renderer together held 33 of
the evening's 66 minutes — half the evening.

That oscillation is the essay being written. It is not distraction: it is one piece of
work carried across two windows, a draft in Word and its source rendered beside it.
Under the old floor none of it reached the model. What survived was twelve entries —
Claude, a Settings window, and a YouTube video about Tetris — so a model asked about
the evening would have described it as those things, confidently, and been wrong about
what you actually did.

So a duration floor cannot sit on an episode. An episode ends whenever the foreground
moves, which means the thing the floor was being applied to is not a unit of work.

## What was built

### `crates/oh-processing/src/attention.rs`, new

Measurement, not judgement. Two things live here.

**Bands and counts.** `Attention` carries active time and visit counts split three ways
— under ten seconds (a window crossed on the way somewhere), ten seconds to a minute,
and a minute or more — plus switches, switches per hour of *active* time, distinct
applications, per-application visit patterns, and the top alternating pairs. A pair is
counted in both directions and reported once, because a person shuttling between a
document and its source crosses both ways and neither direction is the interesting one.

**Threads.** The unit the prompts now name. A `Thread` is a continuous piece of work
carried out in one or two windows.

It is built in **two passes, and the order is the whole trick.** The first pass counts
how often each pair of applications handed the foreground straight to the other across
the entire window, and matches the strongest pairs first, each application taking at
most one partner. The second pass walks the stream with every episode labelled by its
pair and cuts a new thread where the label changes for good.

Deciding the pairing while walking — letting whatever window turned up second become
the partner — was the first attempt and it failed badly: the thread locked onto the
first coincidence it met, and every later visit arrived as an interruption of a thread
that never ended. It produced one stretch holding **270 interruptions**, and the
essay appeared nowhere. That is why `pair_up` is a separate pass, and there is a test
named `an_application_is_paired_with_whichever_it_crossed_with_most` holding it there.

A window that takes the foreground and gives it back within six visits is an
`Interruption`; one that keeps it, or holds it past two minutes of consecutive time
away, ends the thread. A gap of more than fifteen minutes ends it regardless.

The floor — `MIN_THREAD_MS`, still 60 seconds — is applied to the finished thread. Ten
seconds in Windows Terminal is a thread of ten seconds and is still dropped, so the
original bug stays fixed. A hundred four-second returns to one document is a thread of
half an hour and is named.

A thread carries `crossings`, `interruptions` (with counts, time and title), `span_ms`
against `active_ms`, and `mean_uninterrupted_ms` — the number that says whether a
stretch could be thought in.

`between_hours` slices a day by local hour, so "the evening" is answerable without
making a model do arithmetic on timestamps.

**On the real evening, before and after:**

| | before | after |
|---|---|---|
| share of active time inside a named stretch | 32% | **81%** |
| the essay | absent | 19:46–20:26, Word with Markdown Renderer, 33m, 196 crossings, broken into 57 |

### `crates/oh-inference/src/prompt.rs`, changed

- `MIN_EPISODE_MS` is gone. Threads are rendered instead. A one-window, one-visit
  thread renders as the old episode line did, so a thin hour is unchanged, and an hour
  with no thread at all falls back to listing its episodes rather than handing the
  model an empty list.
- An attention block goes into the hour, day and chat prompts, stating the measured
  numbers as facts. It carries one sentence whose only job is to stop the bands being
  read as a verdict: *a short visit is not the same as a short piece of work.*
- `SYSTEM` was rewritten. The sentence that said *anything that held under a minute is
  a window that was touched rather than work that was done: leave it out* is gone — it
  was the same mistake in prose, and it would have told the model to ignore the essay
  even once the data reached it.
- `SYSTEM` and `CHAT_SYSTEM` gained a section on reading switching. Its core rule: a
  high switch count is not distraction on its own, and what separates the readings is
  which windows were trading the foreground and whether the work resumed. The model
  must use the given counts and never estimate its own.

The rule that the model may not invent a mood, purpose or urgency is untouched, and the
new instructions sit inside it rather than around it.

### `src/views/DayView.tsx`, comment only

`MIN_APP_MS` there is a floor on a whole day's use of one application, not on an
episode, so it survives — an application entered a hundred times for four seconds still
clears a minute in total and still gets its row. Its comment claimed the prompts applied
the same floor, which is no longer true, so the comment was corrected. No behaviour
changed.

## The test

`crates/oh-inference/tests/luna_distraction.rs`, ignored by default so a normal
`cargo test` never spends money or needs a key. Two tests:

- `the_evening_measures_as_expected` needs no key and costs nothing. It prints the
  bands, the switch rate and every thread found. This is the fastest way to see whether
  the threading is finding the work, and it is where the 270-interruption bug showed
  itself.
- `luna_answers_how_distracted_the_evening_was` asks Luna the question five ways and
  writes the answers to `drafts/luna-distraction/`.

The key comes from the Windows Credential Manager through `keyring`, exactly as the
application reads it. No key is written down anywhere in this repository.

```bash
cargo test -p oh-inference --test luna_distraction -- --ignored --nocapture
```

## State at the end

Run on this tree and passing: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace` (355 passed, 0 failed),
`npx tsc --noEmit`. The release binary was built with `npm run tauri build --
--no-bundle`.

`npm test` and the browser pass were **not** run: nothing here touches the frontend
beyond one corrected comment, and you asked for no testing. The two ignored desktop
gates, `live_desktop` and `persistence`, were not run either — nothing here touches the
collector.

**The app was not reinstalled.** Built and pushed, per your standing preference that
the reinstall is yours. `target/release/openhistory-win.exe` is the new binary; the
installer hang in `todo.md` is untouched.

### Worth knowing

- The `57` interruptions inside the essay stretch are the real distraction signal, and
  they were invisible before this. Claude 18 times, Windows Explorer 18, Google
  Calendar in Chrome 13, leaving about 34 seconds of work between one and the next.
- `MIN_COUPLING` is 4 crossings. On this evening the real pair crossed 188 times and
  the next pair down crossed 9, so the threshold is nowhere near anything real. If a
  day ever pairs two windows that were not coupled, that is the constant to look at.
- Writing to `prompt.rs` through a shell heredoc mangles backslashes, which silently
  broke one edit and left `SYSTEM` unchanged while reporting success. Anything with a
  Rust line continuation in it should be written to a file and spliced by line number,
  not pattern-matched through a shell.
