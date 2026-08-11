---
name: Bug report
about: Something renders wrong, reads wrong, or doesn't work
title: ""
labels: bug
---

## Which panel?

<!-- Hosts / Containers / Repos / Runners / Usage / Azure Cost / Sentry Crons /
     Services / OpenClaw / Settings — or "the whole window".
     The panels poll and fail independently, so this is the single most useful
     line in the report. -->

## What happened

<!-- What the panel showed. A screenshot is worth a lot here. -->

## What you expected

<!-- Especially: if a value showed as `—`, did you expect a number, or a
     dimmed `0`? Those mean different things and the difference is usually the
     bug. -->

## Environment

- OS and version:
- Built from (commit or branch):
- Relevant panel configured how? <!-- e.g. "GitHub token set, org set, 4 repos tracked" -->

## Anything in the console?

<!-- ./dev run prints poll failures to stderr. Paste anything that looks
     related — but check it for tokens first. -->
