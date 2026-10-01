# Demo hosting plan (public site, $0)

Recorded 2026-10-01. Applies when the web product exists (milestones
19–20); the engine and benchmarks do not depend on it.

## Decision

- **Frontend:** GitHub Pages at `sealion.tmarhguy.com` — static
  React/TypeScript build.
- **Public demo backend:** Render Free Web Service running the Rust SeaLion
  API (Docker). 750 free instance-hours/month; sleeps after 15 minutes of
  inactivity.
- **Demo index:** ship a small prebuilt index with the deployment (or
  rebuild/download at service start). Never the 1M/10M-document benchmark
  corpora.

```text
sealion.tmarhguy.com
        │
 GitHub Pages (React/TypeScript)
        │
        ▼
Render Free API (Rust SeaLion)
        │
        ▼
small prebuilt demo index
```

## Explicit non-choices

- **Cloudflare Workers for the engine:** rejected — 10 ms CPU per request
  and 128 MB memory fight exactly the workload being demonstrated.
- **Railway as primary host:** rejected — permanent free plan is ~$1/month
  usage credit (plus initial $5 trial); not a base to design around.

## Scale claims stay local

The public instance proves the system *works*. The 1M/10M-document
benchmarks, multi-node cluster tests, and failure demos run locally/on
Penn machines for videos and published results; the repo shows those
numbers, the site gives recruiters an instant working demo.
