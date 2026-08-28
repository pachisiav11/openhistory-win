# Handoff — attention and distraction

Written 2026-08-27 in the evening, before the work started; rewritten at the end of the
first pass on 2026-08-28 morning; rewritten again the same day after you picked a
draft and asked for the framing to be simplified further. This covers one task only:
making the summaries and the day chat able to say something true about how scattered a
stretch of work was. The project-wide handoff is `HANDOFF.md` and is unchanged by this.

Read "The finding that shaped it" first. It is the reason the change is larger than a
prompt edit, and if it is ever undone the feature quietly stops working while still
appearing to.

## Where things stand

You picked **draft 2, verdict-first**, and asked for that framing to come out of a
bare question with no per-request steering. It is no longer a draft — `CHAT_SYSTEM` in
`prompt.rs` was rewritten so the chat always opens with a plain one-sentence verdict
before the evidence, unprompted. Asking the day-chat exactly *"How distracted was I
while working on the critical in the evening today?"* now gets that shape by default.

You then asked for the measurement itself to be simplified: keep the pairing threshold,
drop the floor to five seconds, and remove the glance/settled bands and the cap on how
long an interruption is allowed to run. That is done too — see "What changed after the
first pass" below.

`drafts/luna-distraction/` still holds five regenerated answers, now produced against
the simplified measurement and the baked-in verdict-first prompt, for your own
reference. There is nothing left to choose between them for — they were regenerated to
confirm the shipped framing still reads well under the new numbers, not to pick a
winner again.

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

**Counts.** `Attention` carries active time, visits, switches, switches per hour of
*active* time, distinct applications, per-application visit patterns (`AppAttention`),
and the top alternating pairs. A pair is counted in both directions and reported once,
because a person shuttling between a document and its source crosses both ways and
neither direction is the interesting one. (The first pass split this by how long each
visit ran — under ten seconds, ten seconds to a minute, a minute or more. That banding
was removed in the second pass; see "What changed after the first pass".)

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
`Interruption`; one that keeps it ends the thread. A gap of more than fifteen minutes
ends it regardless. (The first pass also ended a thread if the interrupting time
exceeded two consecutive minutes, regardless of whether it came back. That cap was
removed in the second pass — see below.)

The floor — `MIN_THREAD_MS`, now 5 seconds, was 60 — is applied to the finished thread.
A thread below the floor is still dropped, so the original bug (Windows Terminal
touched for ten seconds reported as five minutes of command-line work) stays fixed. A
hundred four-second returns to one document is a thread of half an hour and is named.

A thread carries `crossings`, `interruptions` (with counts, time and title), `span_ms`
against `active_ms`, and `mean_uninterrupted_ms` — the number that says whether a
stretch could be thought in.

`between_hours` slices a day by local hour, so "the evening" is answerable without
making a model do arithmetic on timestamps.

**On the real evening, before and after the first pass:**

| | before | after |
|---|---|---|
| share of active time inside a named stretch | 32% | 81% |
| the essay | absent | 19:46–20:26, Word with Markdown Renderer, 33m, 196 crossings, broken into 57 |

That 81% and 57 are from the 60-second floor with the two-minute interruption cap. The
current numbers, after removing both, are in "What changed after the first pass".

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

## What changed after the first pass

Two rounds of feedback landed after the drafts went out.

**The verdict-first framing was baked in.** You liked draft 2 best, but you didn't want
to steer for it per request — you wanted the bare question, exactly as the app already
sends it, to come out reading that way. `CHAT_SYSTEM` was rewritten with a new opening
instruction: answer with one plain sentence of verdict before any evidence, and for a
distraction question that sentence must itself say how distracted the person was and
what broke in, rather than leaving the reader to work it out from a paragraph of
numbers. `SYSTEM` was left as it was; only the interactive chat prompt needed this,
since the day-chat is where a bare question like this one arrives. Covered by
`the_chat_system_prompt_asks_for_a_direct_answer_before_the_evidence`.

**The measurement was simplified**, on explicit instruction to keep the pairing
threshold and drop the rest: `MIN_THREAD_MS` from 60 seconds to 5, and `GLANCE_MS`,
`SETTLED_MS` and `MAX_INTERRUPTION_MS` deleted along with the banding and the
interruption-length cap they drove. `MIN_COUPLING` (4 crossings) is untouched — see
"Worth knowing" for why it didn't need to move.

One consequence worth flagging rather than quietly shipping: without a cap on how long
an interruption is allowed to run, a long visit elsewhere can now be folded into a
thread as an "interruption" as long as it eventually comes back, rather than ending
the thread and standing as its own stretch. On the real evening this happened once — a
four-minute visit to Settings, previously its own standalone thread, is now counted as
an interruption of the surrounding Claude/Chrome thread — and it is the reason the
share of active time inside a named stretch reads lower now (72%) than it did under the
capped version (81%): that time moved from a thread's own active time into another
thread's interruption time, which the model is told about but which doesn't count
toward "time spent working." This is the direct, intended effect of the simplification
you asked for, not a defect, but it is the one place where "no cap" trades away
something the cap used to catch.

Re-measured on the same evening after both changes:

| | after 60s floor + 2min cap | after 5s floor, no cap |
|---|---|---|
| share of active time inside a named stretch | 81% | 72% |
| the essay | 19:46–20:26, 33m, 196 crossings, broken into 57 | 19:48–20:26, 31m, 194 crossings, broken into 55 |
| stretches found in the evening | not counted this way | 10 |

The essay itself is barely changed — this is the same finding holding up under a
different edge policy, not a different finding.

### `src/views/DayView.tsx`, comment only

`MIN_APP_MS` there is a floor on a whole day's use of one application, not on an
episode, so it survives — an application entered a hundred times for four seconds still
clears a minute in total and still gets its row. Its comment claimed the prompts applied
the same floor, which is no longer true, so the comment was corrected. No behaviour
changed.

## The test

`crates/oh-inference/tests/luna_distraction.rs`, ignored by default so a normal
`cargo test` never spends money or needs a key. Two tests:

- `the_evening_measures_as_expected` needs no key and costs nothing. It prints active
  time, switches, switches per hour, the threaded share and every thread found. This is
  the fastest way to see whether the threading is finding the work, and it is where the
  270-interruption bug showed itself.
- `luna_answers_how_distracted_the_evening_was` asks Luna the question five ways and
  writes the answers to `drafts/luna-distraction/`.

The key comes from the Windows Credential Manager through `keyring`, exactly as the
application reads it. No key is written down anywhere in this repository.

```bash
cargo test -p oh-inference --test luna_distraction -- --ignored --nocapture
```

## State at the end

Also resolved this round: you asked to give the summariser chat context of past
messages. Traced the whole path — `DayView.tsx` to `chatAboutDay` to the
`chat_about_day` Tauri command to `InferenceService::chat` to the "Earlier in this
conversation" block `chat_prompt` already builds — and confirmed the interactive
day-chat already carries the last `MAX_CHAT_TURNS` (8) exchanges forward on every
question. You confirmed that's what you meant and no change was needed, so none was
made. The other reading — giving the automatic hour/day write-up visibility into a
chat that happened separately — was not what was meant; noted here so it isn't
re-investigated from scratch if it comes up again.

Run on this tree and passing, this round: `cargo fmt --all -- --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace` (357 passed, 0
failed, 14 ignored — the desktop-gate and paid-API tests), `npx tsc --noEmit`. The
release binary rebuilt clean with `npm run tauri build -- --no-bundle`:
`target/release/openhistory-win.exe`.

Pushed to `origin/attention-and-distraction`, branched off `main` rather than committed
to it. Open a pull request or merge it locally, whichever you prefer:

    https://github.com/pachisiav11/openhistory-win/pull/new/attention-and-distraction

`npm test` and the browser pass were **not** run: nothing this round touches the
frontend at all. The two ignored desktop gates, `live_desktop` and `persistence`, were
not run either — nothing here touches the collector.

**The app was not reinstalled.** Built and pushed, per your standing preference that
the reinstall is yours. `target/release/openhistory-win.exe` is the new binary; the
installer hang in `todo.md` is untouched.

### Worth knowing

- The interruptions inside the essay stretch are the real distraction signal, and they
  were invisible before this work. 55 of them now (57 under the old cap), Claude and
  Windows Explorer the two most frequent sources, leaving about half a minute of work
  between one and the next.
- `MIN_COUPLING` is 4 crossings, unchanged through both passes at your instruction. On
  this evening the real pair crossed roughly 190 times and the next pair down crossed
  9, so the threshold is nowhere near anything real. If a day ever pairs two windows
  that were not coupled, that is the constant to look at.
- Removing the interruption-length cap means a long excursion can now be absorbed into
  a thread as an interruption rather than standing as its own stretch, as long as the
  work comes back within six visits — see "What changed after the first pass" for the
  one case this changed on the real evening. Worth remembering if a threaded share ever
  looks lower than expected: check whether something substantial got folded in as an
  interruption of something else before assuming a measurement bug.
- Writing to `prompt.rs` or `attention.rs` through a shell heredoc mangles backslashes,
  which silently broke one edit and left `SYSTEM` unchanged while reporting success,
  and recurred once more during the second pass. The reliable fix: write the
  replacement block to a plain file with no shell involved, then splice it into the
  target by exact line number with a small script that asserts the anchor line's
  content first. Anything with a Rust line continuation in it should go through that
  path, not a heredoc or an in-shell string replace.
