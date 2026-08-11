# Solador

> **⚠️ Superseded — see #20.** This document describes the original cloud-estate
> direction (Vercel/Neon/Cloudflare service monitoring) that was deferred. The
> shipped product is a local-first host/CI/worktree cockpit. For the current
> architecture, see the [README](../README.md) and [CLAUDE.md](../CLAUDE.md).
> This file is retained for historical context only.

**Product Requirements Document**

| | |
|---|---|
| **Version** | 1.0 |
| **Date** | December 2024 |
| **Status** | Draft |
| **Product** | Sassy Dog |

---

## Executive Summary

Solador is a native macOS dashboard application that provides developers with a unified, glanceable view of their development infrastructure. The app monitors local git repositories for drift from remote branches and aggregates status information from cloud services including GitHub, Vercel, Neon, and Cloudflare.

The core value proposition is reducing context-switching and cognitive load for developers who manage multiple repositories and cloud resources. Rather than bouncing between GitHub, Vercel dashboards, and terminal windows, developers can see their entire estate's health at a glance—ideally on a second monitor while working.

---

## Problem Statement

Modern developers, particularly those using AI coding assistants for "vibe coding," juggle significant complexity: multiple git repositories, deployment pipelines, CI workflows, and database resources. The current workflow requires manually checking multiple dashboards and hoping to notice when something breaks.

### Key Pain Points

- **Git drift awareness:** Developers don't realize how far behind main they've drifted until merge conflicts hit
- **Context fragmentation:** Status information scattered across GitHub, Vercel, Neon, and Cloudflare dashboards
- **Delayed failure detection:** Build failures and deployment issues discovered minutes or hours after they occur
- **Terminal context switching:** Manually navigating to project directories interrupts flow state

---

## Target Users

### Primary: Solo Developers & Indie Hackers

Independent developers building and shipping products. They manage multiple projects simultaneously and need efficient tooling to stay on top of their infrastructure without dedicated DevOps support.

### Secondary: Small Startup Teams

Engineering teams at early-stage startups (2-10 developers) where everyone wears multiple hats. They benefit from shared visibility into deployment and infrastructure status.

---

## Product Vision

A persistent, glanceable dashboard that answers: "Is my stack healthy right now?" The interface uses a simple red/green/orange status model—developers don't need graphs and metrics, they need to know if anything is on fire while they're deep in flow.

### Design Principles

- **Glanceable:** Status visible at arm's length on a second monitor
- **Actionable:** Click to open terminal, browser, or Claude Code in the right context
- **Native:** macOS-native look and feel using Swift/SwiftUI
- **Minimal:** Problems surface to the top; healthy resources stay quiet

---

## MVP Scope

### Local Git Monitoring

Core functionality for tracking local repository state against remote branches.

- Add repositories via drag-and-drop or folder browser
- Monitor git state using FSEvents for change detection
- Compare local HEAD to remote tracking branch
- Display states: in sync, ahead by N, behind by N, diverged, uncommitted changes
- Click action: open configured terminal at repo path, or launch Claude Code

### GitHub Workflows

Integration with GitHub Actions for CI/CD visibility.

- OAuth authentication flow
- Display workflow runs for connected repositories
- States: passing, failing, running, queued
- Click action: open workflow run in browser
- Scope: most recent run per workflow

### Vercel Deployments

Integration with Vercel for deployment status.

- OAuth authentication flow
- Display projects and latest deployment status
- States: ready, building, error, queued
- Click action: open deployment or project in Vercel dashboard

### Neon Databases

Integration with Neon for database status.

- API token authentication
- Display projects and branch status
- States: active, idle, error
- Optionally show compute suspended vs active status
- Click action: open Neon console

### Cloudflare Resources

Integration with Cloudflare Workers and Pages.

- API token authentication
- Workers: deployment status, last deployed timestamp
- Pages: latest deployment status
- States: active, deploying, failed
- Click action: open in Cloudflare dashboard

### Global UX Requirements

- Standard macOS window (designed for second monitor use)
- Configurable refresh interval (30s / 1m / 5m)
- Manual refresh button
- Problems surface to top with visual emphasis
- Dark mode default (likely only mode for v1)
- Minimal chrome—designed to run all day

### Repository Auto-Linking

Automatically suggest connections between local repos and cloud services.

- Parse git remotes to detect GitHub owner/repo
- Match Vercel projects by GitHub repo connection or project name
- Present suggestions with confidence indicators (exact match vs suggested)
- Allow manual override and custom linking

---

## Out of Scope for MVP

- Team sync / shared dashboards
- Historical data / trends
- Notifications / alerts (potential v1.1)
- Custom integrations / plugins
- Windows or Linux versions
- Detailed logs or drill-down views
- AWS / GCP / Azure integrations

---

## Technical Architecture

### Technology Stack

| Component | Technology |
|---|---|
| Platform | macOS (native) |
| Language | Swift |
| UI Framework | SwiftUI |
| Data Persistence | SwiftData |
| Credential Storage | macOS Keychain |
| File Monitoring | FSEvents |
| OAuth Flow | ASWebAuthenticationSession |

### Authentication Strategy

| Service | Auth Method | Rationale |
|---|---|---|
| GitHub | OAuth + PKCE | Excellent OAuth support, users expect browser flow |
| Vercel | OAuth + PKCE | Standard integration path for Vercel |
| Neon | API Token | Token-based API, no OAuth available |
| Cloudflare | API Token | Token-based is their standard model |

### Data Model

**Persisted (SwiftData)**

- **TrackedRepository:** path, name, linked GitHub/Vercel IDs
- **ServiceConnection:** service type, account identifier, validation timestamp
- **AppSettings:** refresh interval, preferred terminal, launch preferences

**Runtime State (transient)**

- **GitStatus:** ahead/behind counts, uncommitted changes flag
- **VercelDeployment:** current deployment state per project
- **WorkflowRun:** latest CI run status per workflow
- **NeonStatus / CloudflareStatus:** current resource states

### Security Considerations

- All credentials stored in macOS Keychain
- OAuth scopes limited to minimum required (read-only where possible)
- PKCE flow for OAuth to avoid client secret exposure
- Token validation on app launch and before sensitive operations
- No telemetry or analytics that includes credentials

---

## Business Model

### Pricing Strategy

Free for personal use, paid for commercial use. Honor system similar to Sublime Text. This approach prioritizes adoption and community building over aggressive monetization, while still capturing value from professional users and businesses.

### Distribution

- Direct download from product website
- Potential Mac App Store listing (future)
- Payment processing via Paddle (merchant of record)

---

## Development Timeline

| Week | Deliverables |
|---|---|
| Week 1 | **Foundation + Git Monitoring** — Project setup, basic window, GitMonitor service, repo management, terminal launcher |
| Week 2 | **GitHub Integration** — OAuth flow, KeychainStore, GitHubService, workflow status UI, auto-linking |
| Week 3 | **Vercel + Neon Integration** — Vercel OAuth, VercelService, Neon API token auth, NeonService, infrastructure UI |
| Week 4 | **Cloudflare + Polish** — CloudflareService, settings view, onboarding flow, error handling, empty states |

---

## Success Metrics

1. **Adoption:** 1,000 downloads within first 3 months
2. **Engagement:** Daily active usage (app open >4 hours/day) for 40% of users
3. **Retention:** 60% of users still active after 30 days
4. **Conversion:** 5% conversion to paid commercial license within 6 months
5. **Quality:** Crash-free rate >99.5%

---

## Risks and Mitigations

1. **API Changes:** Cloud services may change APIs. Mitigation: Use stable API versions, implement graceful degradation.
2. **OAuth App Approval:** GitHub/Vercel may have review processes. Mitigation: Apply early, follow guidelines, have API token fallback.
3. **Rate Limiting:** Aggressive polling could hit API limits. Mitigation: Configurable intervals, smart caching, exponential backoff.
4. **Competitive Response:** Vercel/GitHub could build similar features. Mitigation: Focus on cross-platform aggregation value, not single-service features.
5. **Honor System Revenue:** Low conversion on honor-based payment. Mitigation: Focus on building community and reputation first; adjust model if needed.

---

## Future Considerations

Potential features for post-MVP releases:

1. **Team Backend:** Shared dashboards, team visibility, role-based access
2. **Notifications:** macOS notifications for status changes, configurable alert thresholds
3. **Additional Integrations:** Netlify, Railway, Supabase, PlanetScale, AWS Lambda
4. **Historical Trends:** Build time trends, deployment frequency, uptime tracking
5. **Custom Actions:** User-defined scripts triggered from dashboard
6. **Menu Bar Mode:** Compact menu bar presence with dropdown for quick status

---

## Appendix: Terminal Support

Supported terminal applications for context-aware launching:

| Terminal | Bundle Identifier |
|---|---|
| Terminal | com.apple.Terminal |
| iTerm | com.googlecode.iterm2 |
| Warp | dev.warp.Warp-Stable |
| Ghostty | com.mitchellh.ghostty |
