# Setting up GitHub

Solador's **Repos** and **GitHub Runners** panels read GitHub with a
**fine-grained personal access token**, read-only. There is no OAuth flow: you
paste a token into the app, and it is stored in your OS credential store —
Keychain on macOS, Credential Manager on Windows — never in the settings file.

A token belongs to an **account**, and you can add more than one (a work account
and a personal one, say). Each account fetches the repos its own token was
granted, and polls the organizations it watches.

## 1. Create a fine-grained PAT

1. Go to **GitHub → Settings → Developer settings → Personal access tokens →
   Fine-grained tokens** (<https://github.com/settings/personal-access-tokens>).
2. Click **Generate new token**.
3. Set **Resource owner** to yourself, or to the organization whose repos and
   runners you want to watch. Watching an org's self-hosted runners requires a
   token owned by that org.
4. Scope it to the repositories whose CI you want to watch, or to all of them.
5. Under **Repository permissions**, grant **read-only** access to:
   - **Actions** — required; reads workflow runs.
   - **Contents** — the Repos panel's remote branch counts.
   - **Issues** — the Repos panel's open-issue counts.
   - **Pull requests** — the Repos panel's open-PR counts.
   - **Metadata** — read-only, granted automatically alongside the others.
6. Under **Organization permissions**, also grant read-only:
   - **Self-hosted runners** — the GitHub Runners panel. Without it that
     panel has nothing to read even though the Repos panel works fine.
   - **Projects** — the Repos panel's READY column (open issues whose
     project-board Status is `Ready`). Without it that one column reads `—`
     and the panel's footer says so; everything else is unaffected.

   Both are easy to miss: they are *organization* permissions, not
   repository ones, and only appear when the token's resource owner is an
   organization.
7. Leave everything else at **No access**. Solador never writes to GitHub.
8. Generate the token and copy it — it is shown only once.

## 2. Add the account to Solador

1. Open **Settings → Sources → Add source → GitHub**. The steps are the token,
   then the organizations to watch, then Ready.
2. On the first step, give the account a name — `work` or `personal`, whatever
   you'll recognise — and paste the token into **Fine-grained PAT**. A token is
   needed to continue.
3. On the second step, list the organizations whose runners you want watched,
   separated by commas (see step 4), or leave it empty to watch none.
4. **Finish** saves the account, its token and its organizations together.
   **Back** and **Cancel** save nothing: no account exists until Finish.

To swap a token, rename the account or change its organizations later, click the
account's row under **Settings → Sources** and walk the same steps; leaving the
token empty keeps the one stored. **Remove source** on its first step deletes
only that account, with its token — deleting it also drops the repos and orgs
you configured under it, and the last step lists them before you confirm.
Finish lands on the account's card, where the repos are tracked (step 3).

## 3. Choose which repos to watch

On the account's card (it opens after Finish, or from **Watched repos…** on the
account's steps), **Add a repo** searches everything the token was
granted: opening the account reads the list once (the box says so while it
does, then how many repos the token can read), and typing any part of a
repo's `owner/name` lists the matches as you type. Tick one to watch it; the
arrow keys and Enter do the same from the keyboard, and Escape clears the box.
Typing a full `owner/name` the token can't list offers **Add … by name**,
which covers public repos you'd rather not grant the token access to at all.
Watched repos are listed above the box, grouped by owner. Repos you disable
stay in the list and are simply skipped.

Entries are annotated as you'd hope: `watching`, `archived`, `not in this
token's grants` for a repo the token can no longer see, and the other account
that fetches a repo another account already watches.

Each repo can name **watched workflows** (comma-separated, e.g. `release.yml`).
Leave it blank for the default `ci.yml` view, or list the extra workflows whose
failures should redden the panel — matched by display name or filename,
case-insensitively.

## 4. Watch an organization for runners

On the account's steps, type the orgs into **Organizations**, separated by
commas, and press **Finish**. Taking a name out of the list stops watching it.
The card also offers the organizations it found from the repos your token can
read, each with a **Watch** button. Watched orgs are polled with that account's
token, which is why step 1.6's organization permission matters.

An organization can be watched by exactly one account — if you add it to a
second, Solador refuses and tells you which account already has it, rather than
polling the same org twice.

## Troubleshooting

### Repos panel shows no data, or 401
- Check the token hasn't expired, and was scoped to the repos you expect.
- Confirm **Actions** read access. Missing **Issues** or **Pull requests** shows
  `—` in those columns specifically, and a missing organization **Projects**
  permission shows `—` under READY with a footer naming it, without breaking
  the rest of the panel — which is the intended behaviour, not a failure.
- Replace the token from the account's steps (Settings → Sources → the account).

### Runners panel is empty but Repos works
Almost always the missing **Organization permissions → Self-hosted runners**
read grant from step 1.6, or a token whose resource owner is you rather than the
organization. Both are fixed by generating a new token, not by reconfiguring
Solador.

### An organization won't add
It is already watched by another account. The message names which one, and
nothing is saved until you take it out of the list.

### Rate limiting
Authenticated requests get **5,000/hour per token**. The Repos panel spends
about **4 requests per repo per pass**, so roughly **20 repos at the default 60s
cadence**, or about **100 at a 5-minute** cadence. If panels stop updating,
either lengthen the **Poll cadence** in the Configure of a GitHub Repos or GitHub Runners tile (one cadence, shared by both),
watch fewer repos, or split them across two accounts with separate tokens.
