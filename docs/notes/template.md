# Title: what the work was, in a few words

**Status:** measured; nothing changed / measured, not shipped / shipped in pages-vYYYYMMDD-n or PR #… / code merged, not deployed · **Date:** D Month YYYY · **Measured on:** the index version, commit, database read or sample every number below comes from, with when it was read · **Code:** the crates, scripts and design sections · **Issues:** #…

When the subject changes later (shipped, deployed, retired, superseded), add an **Outcome:** line under the status line with the date and a link to the PR, the current document or the later note. Leave the rest of the note as it was.

## The question

What we wanted to know or change, and why it mattered: the search that was slow, the pages that couldn't be found, the count that was wrong. One or two paragraphs. Link the design section that describes the mechanism as it now stands rather than describing it again.

## What we found

The answer in a few sentences, for a reader who stops here. Optional for a short note.

## What we did

The change or the experiment, in the order it happened. Decisions and the alternatives not taken, briefly; a decision big enough for an ADR gets one. A measurement-only note can call this section "Method".

## What we measured

| What | Before | After | How                                                                                             |
| ---- | ------ | ----- | ----------------------------------------------------------------------------------------------- |
| …    | …      | …     | the script, command or endpoint, and the environment (cold or warm, the job's 2 vCPU, a laptop) |

Say what the numbers don't show as well as what they do, under "Caveats" if there are several.

## What is left

Open questions, the next measurement to take, and what would make this note wrong.
