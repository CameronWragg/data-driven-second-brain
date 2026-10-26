---
name: remember
description: Review this session for knowledge worth keeping in the Second Brain and record it. Use when the owner says "remember this", "save that to the brain", or runs /brain:remember, or at the end of a long task.
---

# Remember (PROTOTYPE)

1. Call `context` if you haven't this session.
2. List candidate Recollections from this session: decisions (and the alternatives rejected), conventions, caveats (symptom, cause, avoidance), architecture facts and how-tos. Apply the server instructions' "do not write" list strictly; most sessions yield zero to two.
3. For each candidate, `search` with `entities` set to what it's about. If an existing Recollection covers it, plan an `update` or `supersede` instead of a `create`.
4. Show the owner the plan as one line per item (kind, title, create/update/supersede) and wait for their approval. If they named a specific fact ("remember X"), skip the approval and write that.
5. Send one `write` batch. Anchor anything about specific code. On a duplicate rejection, follow the decision table.
6. Report the commit and one line per Recollection written.
