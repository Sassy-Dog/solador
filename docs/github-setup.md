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
6. **For the GitHub Runners panel only**, also grant, under **Organization
   permissions**:
   - **Self-hosted runners** — read-only.

   This one is easy to miss: it is an *organization* permission, not a
   repository one, and without it the Runners panel has nothing to read even
   though the Repos panel works fine.
7. Leave everything else at **No access**. Solador never writes to GitHub.
8. Generate the token and copy it — it is shown only once.

## 2. Add the account to Solador

1. Open **Settings → Accounts**.
2. Under **Add Account**, choose the vendor (**GitHub**), give it a name — `work`
   or `personal`, whatever you'll recognise — and paste the token into
   **Fine-grained PAT**.
3. Click **Add Account**. The card shows **Token stored** once it's saved.

You can add the account first and its token later; the card will say **No
token** until you do. To swap a token later, use **Replace token** on the card
rather than deleting the account — deleting it also drops the repos and orgs
you configured under it.

## 3. Choose which repos to watch

On the account's card, **Configure repos…** opens a picker that lists
everything the token was granted — it reads them live, so you don't have to type
names. The same panel takes an `owner/name` slug plus **Add**, which covers
public repos you'd rather not grant the token access to at all. Repos you
disable stay in the list and are simply skipped.

Entries are annotated as you'd hope: `archived`, and `not in this token's
grants` for a repo the token can no longer see.

Each repo can name **watched workflows** (comma-separated, e.g. `release.yml`).
Leave it blank for the default `ci.yml` view, or list the extra workflows whose
failures should redden the panel — matched by display name or filename,
case-insensitively.

## 4. Watch an organization for runners

Also on the account's card, type the org into **Organization** and click
**Watch** — **Stop watching** removes it. Watched orgs are polled with that
account's token, which is why step 1.6's organization permission matters.

An organization can be watched by exactly one account — if you add it to a
second, Solador refuses and tells you which account already has it, rather than
polling the same org twice.

## Troubleshooting

### Repos panel shows no data, or 401
- Check the token hasn't expired, and was scoped to the repos you expect.
- Confirm **Actions** read access. Missing **Issues** or **Pull requests** shows
  `—` in those columns specifically, without breaking the rest of the panel —
  which is the intended behaviour, not a failure.
- Replace the token from the account card.

### Runners panel is empty but Repos works
Almost always the missing **Organization permissions → Self-hosted runners**
read grant from step 1.6, or a token whose resource owner is you rather than the
organization. Both are fixed by generating a new token, not by reconfiguring
Solador.

### An organization won't add
It is already watched by another account. The message names which one.

### Rate limiting
Authenticated requests get **5,000/hour per token**. The Repos panel spends
about **4 requests per repo per pass**, so roughly **20 repos at the default 60s
cadence**, or about **100 at a 5-minute** cadence. If panels stop updating,
either lengthen the cadence in **Settings → General → Panel Poll Cadence**,
watch fewer repos, or split them across two accounts with separate tokens.
