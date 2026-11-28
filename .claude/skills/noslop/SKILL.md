---
name: noslop
description: Edit or audit text that people outside this session will read (site copy, volunteer task instructions, READMEs, issue and PR text, emails, list posts, reports, grant text) so it reads like a careful person wrote it for a specific reader, keeps every fact and its certainty, and follows this project's house rules. Use before any external copy ships, when asked to "noslop" or de-slop something, or when asked whether a draft reads as AI.
---

# noslop

Make the text read like a specific person wrote it for a specific reader. Keep every fact and how sure it is. Invent nothing.

Built from three public skills: petergyang/no-ai-slop (edit and detect jobs, voice preservation), phuryn/work-humanizer `noslop` (fact list, certainty rules, interview mode, mechanical sweep) and SalvatoreCampagnese/unslop-skill (fix, never accuse; what not to flag; what to put back). The house rules below come from the maintainer's corrections and override the general rules.

## Three jobs

**Edit** (default). You get a draft. Return the edited text, then a short **What changed** list (at most five bullets) and a **Verify before sending** list for every claim you could not trace to a source.

**Audit.** Asked whether something reads as AI, or to flag without rewriting. For each problem, quote the line, name the tell, give the fix in a few words. Do not rewrite, score, or guess who or what wrote it. A tell is a default nobody chose, not proof of authorship. Offer to edit after.

**Interview.** Use when asked, or when the draft fails this test: could anyone with the same one-line request have written it? Then it lacks what only the writer knows (the real number, what happened, what they think) and editing cannot supply it. Ask one round of at most five questions, each answerable in under a minute, each quoting the line its answer will replace. Write from the answers in the writer's words. A skipped question gets a plainer sentence or a cut, never an invented detail.

If you have no draft, ask for it. If the venue is unclear and you cannot infer it, ask one question: who reads this, and where?

## House rules (these win)

1. **No em dashes.** Zero, in every venue. No spaced en dashes either. Pick the replacement by job: period for two thoughts, colon for label then description, parentheses for a real aside, comma for a light pause. Do not turn them into semicolons.
2. **No metaphor standing in for a mechanism.** "The index is the backbone of search", "give requests a home": say what the thing does. "Search reads the Quickwit index" says it.
3. **Plain headings.** Noun phrases that name the content ("What the data covers", "How answers are counted"). No headings whose only job is emphasis or cleverness ("Exactly this", "X, handled", "Done right"). Sentence case.
4. **Every claim traces to a source.** A number, date, count, origin story, volunteer anecdote or result must come from the draft, the repo, the data, or the maintainer. If you inferred it, it goes on the Verify list. Never write a backstory to make copy warmer.
5. **Modern terms in the site's own voice.** On the history sites use "free Black" and "enslaved", not census or archival category names, in labels, headings and data columns. Quote historical sources and authors' text verbatim and do not edit them. Note an original category once where accuracy needs it.
6. **Non-commercial voice.** usnewsmap.com sells nothing: no sales register, no "customers".
7. **Public text holds no private infrastructure.** This is a public repo. No hostnames, IPs, container or VM IDs, node names, storage paths, budgets or costs in its issues, PRs, READMEs or site copy.
8. **Measured results stay measured.** Keep the test set, the n, the metric and the direction exactly as reported. A pre-registered result is reported as scored once, with any deviation stated. Never round "3.1% CER on 120 held-out lines" into "much more accurate".

## What you must not change

- **Certainty.** "Could help" is not "cuts". "Typically" is not "always". "On the held-out split" is not "on real pages". Keep one qualifier where the writer had one, and cut the pile around it.
- **Reasons and conditions.** Do not add a cause, an eligibility rule or a benefit the writer did not state, even an obvious one.
- **Who does what.** Keep the actor. "We'll email you" stays with "we".
- **Vague into vaguer.** If a claim is vague and you have nothing specific, keep the writer's words or cut the sentence. Never swap in something vaguer.
- **The reader's working vocabulary.** Keep terms readers type or search for (CER, ALTO, OCR, LCCN, Chronicling America). Cut jargon that only performs expertise.

## How to edit

1. **Read for the job.** Who reads it, where, and what they must know or do after. Answer first for replies, PR summaries and announcements. Evidence first for explanations and reports.
2. **List the facts with their certainty.** Every number, name, date, claim, link, commitment, caveat and call to action, with its qualifier. This list must survive.
3. **Delete before you rewrite.** For each clause ask: is it here for the reader, or to protect the writer from a doubt? Padding is filler, restatement and hype. Voice, concrete detail and the warmth a genre needs stay.
4. **Fix the tells,** strongest first (list below). Leave a sentence with no tell untouched: no synonym swaps, no polish.
5. **Fix rhythm and flow.**
6. **Run the final checks.**

## The tells, strongest first

**A. Staged emphasis that adds no information**

- Contrast with a claim nobody made: "It's not X, it's Y", "not just X but Y", "Y rather than X", "No X, no Y". State Y. Keep the contrast only if readers really believe X.
- Pause-and-point: "Here's the thing:", "Let me be clear:", "Let's break it down", a count announced before a list ("This matters in three ways:"), and the colon reveal ("The best part: it learns."). Make the point the next sentence.
- Faux insight: "What most people miss", "Here's what nobody tells you". Make the claim stand alone.
- Aphorisms, stacked parallel lines and fragment stacks: "Less clicking. More building." "No setup. No config. Just results." At most one crafted line per piece. Zero is fine.
- Fake-profound kickers and recap endings: "In conclusion", "Ultimately", "The bottom line", "And that's why X matters", "Thoughts?". Delete. End on the last concrete point or the next action.
- Invented metaphor labels ("a comfort blanket", "the operating system for X"). Say the plain thing.

**B. Doing the reader's work** (delete the sentence, and if the meaning survives, leave it deleted)

- Spelling out the implication: "This means that", "In other words", and the trailing -ing rider (", highlighting the team's commitment to", ", underscoring", ", reflecting a broader shift").
- Pointing: "Notice how", "As you can see", "That last part matters more than it sounds".
- Telling the reader how to feel: "This is exciting", "This should worry you".
- Defining a word the reader knows, or narrating transitions ("Now let's look at"). One signpost per major section is fine in a long document.

**C. Inflation and generic claims**

- An adjective with nothing behind it: significant, seamless, powerful, robust, comprehensive. Attach the number or mechanism the source gives, or cut it.
- Significance puffery: "a pivotal moment", "a testament to", "plays a vital role", "reshaping the landscape". Keep the fact, drop the significance.
- Portability test: if the sentence could move unchanged to another project, it is filler. Replace it with a fact specific to this one or cut it.
- Copula avoidance and nominalized verbs: "serves as" is "is", "boasts" is "has", "perform an evaluation of" is "evaluate", "posits" is "says".
- Vague authority: "experts agree", "studies show", "many archives". Name the source or cut it. If there is no source, ask.
- Stock vocabulary: see `tells.md`. One hit is weak evidence. Several together are the tell.

**D. Hedges in the wrong place**
State facts flatly and hedge opinions once. Cut filler hedges ("arguably", "it's worth noting", "it could potentially"). When a claim needs a limit, name the case: "On Fraktur pages the old model still wins." Never add an absolute ("always", "the first", "every") the source cannot back.

**E. Rhythm**

- Sentence length: if most sentences sit in one 10 to 20 word band, vary it in the direction it needs.
- Openers: too many sentences starting with The, It, This, That's, You.
- Triads by reflex: three items only when there are three things.
- Shuffle test: if the sentences could be reordered without loss, rebuild the spine so each leans on the last (cause, contrast, example, consequence). Do not sprinkle connectors to fake it.
- Filler transitions: Additionally, Furthermore, Moreover, Importantly, Notably, That said. Plain But, So and And are fine.
- Synonym cycling: one name per thing. If it is "the task" in one sentence, it is not "the item" in the next.

**F. Formatting**

- Bold sprinkled through prose, emoji in headings or bullets, Title Case headings, headers over two-sentence sections.
- `**Label:** text` bullets everywhere when two sentences of prose would read better. Reference documents, runbooks and release notes may keep real labels and lists.
- Markdown leaking where it will not render (site copy, emails, plain-text fields).
- Exclamation marks for warmth.

**G. Chat residue and placeholders**
"Great question!", "Certainly!", "I hope this helps", "Let me know if you'd like", "Here's a revised version:", "As of my last update", unfilled `[Your Name]` or `2026-XX-XX`, `utm_source=chatgpt.com` in links. Remove the wrapper, keep the content.

## Sound like a person, not a costume

Plain is not flat. Write the way a competent person in this role writes this genre: contractions, everyday verbs, "I" or "we" and a direct "you" where the genre uses them. Put back what slop removes: plain "is", "has" and "there is", a short sentence next to a long one, a real number, an opinion with a subject attached.

Do not bolt on personality ("Honestly,", "Quick heads-up", a joke the writer did not make, a chatty rhetorical question). No planted typos or forced slang.

Keep what is already human: specific details, odd word choices, opinions, asides, humor, doubt. If the writer gave a sample, match its sentence length, punctuation and register. The sample beats the rules above except the house rules.

## Do not flag

Perfect grammar, formal register on its own, one transition word, one "robust" in an engineering context ("robust" is fine for an estimator), a word inside a quotation, product name or code, and anything quoted from a historical source.

## Venues

- **Volunteer task text** (check.usnewsmap.com intro, question, rules, guidance, notes, button labels, help lines): written for a careful stranger with no OCR background.
  - Imperative, action first, one instruction per sentence. Say what to do, then the edge case.
  - Name each button exactly as it reads ("Can't tell", "Not text", "Bad crop"), every time.
  - Say each instruction once, where it is needed: all-language rules in `rules`, one language or paper in its intro or guidance, one button next to that button. Delete a sentence that repeats a button label.
  - Locked wording keeps its meaning. Changing it needs a note on the PR, and during a blind test the science thread must see it first. Locked: answer rules that define the data (equivalences such as ſ = s, the en-cover edge-word allowance, the I/J rule), blind-test neutrality (never hint which reading is the machine's, the new model's or the likelier one), and the privacy and licence lines (CC0, random browser id).
  - If locked wording is also confusing, do not fix it in place. Flag it with a proposed rewording that keeps the meaning, for the maintainer or the science thread to approve.
  - Keep the concrete pairs ("n and u", "C and С"). They are the most useful words on the screen.
  - One "Thank you" at the end of a run, not on every screen. The intro and rules together should fit on one phone screen above the task.
  - Configs for languages that live only in the Worker DB come from `/api/languages/<code>` and change through sync, not by hand. Run `node --test edge/check/*.test.mjs` after a copy change. Some tests assert exact strings.
- **Site copy** (usnewsmap.com and its sister sites): what the reader can do here and what the data is, with its source and dates. No "explore", "discover", "journey", "rich history", "unlock".
- **Issue comments and PR bodies:** what changed, why, how it was tested, what is left. Results with their numbers and test set. No "This PR introduces a comprehensive...". Check house rule 7 before posting to this public repo.
- **Reports and study write-ups:** method, data, result, limits, in that order. Name every deviation from the plan.
- **Email and list posts:** the ask or the news in the first two lines, one reason you are writing, one ask. No "I hope this finds you well".
- **Grant text:** what is done, for whom, with what, measured how. Watch field length limits.
- **Chat and thread replies:** the answer first, one to four sentences, background only if the reader lacks it.

## Final checks (every time)

1. **Fact diff.** Every item from step 2 is present with the same value and the same certainty. Nothing new appears unless the writer or a source supplied it.
2. **Mechanical sweep.** Run `python3 .claude/skills/noslop/check.py --file <draft>` (or `--text "..."`). It finds em and en dashes, semicolons, stock vocabulary, staging phrases, chat residue, placeholders and flat rhythm. Eyes miss these. Fix every em dash and judge the rest.
3. **House rules 1 to 8,** one at a time.
4. **Crafted lines:** one or none.
5. **Read it aloud as the writer.** Anything they would not say gets plainer.
6. **Length.** Usually shorter than a padded draft. Never shorter by losing a fact, the voice, or warmth the genre needs.
