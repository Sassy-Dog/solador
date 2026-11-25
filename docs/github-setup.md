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
   separated by commas (see section 4 below), or leave it empty to watch none.
4. **Finish** saves the account, its token and its organizations together.
   **Back** and **Cancel** save nothing: no account exists until Finish.

To swap a token, rename the account or change its organizations later, click the
account's row under **Settings → Sources** and walk the same steps; leaving the
token empty keeps the one stored. **Remove source** on its first step deletes
only that account, with its token. Its repos stay tracked but become
unattributed, and it stops watching its organizations; the last step lists both
before you confirm.
Finish lands on the account's card, where the repos are tracked (section 3 below).

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

## Actions billing token

The **Actions billing** tile reads your organization's Actions minutes, net spend
and Actions budget. GitHub only serves those to an organization admin, so it
uses **its own token**, separate from the account token above. The account token
never gains this permission, and every permission listed in step 1 is unchanged.

An organization admin creates it:

1. **GitHub → Settings → Developer settings → Personal access tokens →
   Fine-grained tokens → Generate new token**.
2. Set **Resource owner** to the organization whose billing you want to read.
   The organization needs GitHub's enhanced billing platform.
3. Under **Organization permissions**, grant read-only **Administration**
   (**Administration: Read**), and nothing else. Leave every repository
   permission at **No access**.
4. Generate the token and copy it; it is shown only once.

Then in Solador: **Settings → Sources → Add source → Actions billing**. Type the
organization's login (not a URL) and paste the token into **Billing token**, then
choose **Finish**. Nothing is saved before that, and an empty organization or
token is refused in its own words. The token is stored in your OS credential
store under its own item, and Remove source forgets it.

## GitHub Copilot token

The **GitHub Copilot** source has two independent parts on the AI Usage tile.

- **Organization** shows the whole pool of an organization **you administer**,
  not your own usage. It needs no token of its own: it reads with the Actions
  billing token and organization above, whose permission is **Organization
  permissions → Administration: Read** (no permission named for billing or
  Copilot does this), so set **Actions billing** up first. Solador refuses to
  turn the part on until that is done, and removing the part never deletes the
  Actions billing token.
- **Personal** covers your own Copilot plan **or a seat an employer assigns you**
  (its limit, what you have used of it this period and when it resets), plus
  anything billed to you personally. The plan and seat figure comes from an
  endpoint GitHub does not document, so if GitHub changes it the row shows `—`
  rather than a wrong figure. It needs a token of its own, created on your
  account:

  1. **GitHub → Settings → Developer settings → Personal access tokens →
     Fine-grained tokens → Generate new token**.
  2. Leave **Resource owner** as yourself.
  3. Under **Account permissions**, grant read-only **Plan** (**Plan: Read**), and
     nothing else. Leave every repository and organization permission at **No
     access**.
  4. Generate the token and copy it; it is shown only once.

Then in Solador: **Settings → Sources → Add source → GitHub Copilot**, tick the
parts you want, paste the token into the personal part and type your GitHub
username, then choose **Finish**. Nothing is saved before that. The token is
stored in your OS credential store under its own item, and Remove source forgets
it.

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

### Actions billing says the credential was rejected
The token needs **Administration: Read** on the organization, and it must be
owned by an organization admin: a token from a non-admin, one whose resource
owner is you rather than the organization, or your account token pasted by
mistake is refused. Create a new one as above and replace it from the source's
steps. A **not found** message means the organization login is wrong or it is
not on GitHub's enhanced billing platform.

### Actions billing says "usage through" a few hours ago
That is GitHub's report lagging, not Solador. It is shown as a plain line and
is not a warning: the tile only warns when a read fails or the figures are
older than the poll cadence expects.

### Copilot says the credential was rejected
For the **organization** part, the Actions billing token needs **Administration:
Read** and an organization admin, as above. For the **personal** part, the token
needs the user permission **Plan: Read**. A **not found** message means the
organization (and its enhanced billing platform) or the username is wrong. The
personal **billing** row only counts usage billed to you personally, so a seat a
company or school provides shows a measured `$0.00` there; the **Copilot ·
\<plan\>** row beside it is the seat's own limit. If that row says the token
belongs to a different account, the token was created on another GitHub account
than the username you typed.

### Rate limiting
Authenticated requests get **5,000/hour per token**. The Repos panel spends
about **4 requests per repo per pass**, so roughly **20 repos at the default 60s
cadence**, or about **100 at a 5-minute** cadence. If panels stop updating,
either lengthen the **Poll cadence** in the Configure of a GitHub Repos or GitHub Runners tile (one cadence, shared by both),
watch fewer repos, or split them across two accounts with separate tokens.
