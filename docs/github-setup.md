# Setting up GitHub Integration

Solador's **Repos** and **GitHub Runners** panels read GitHub data using a
**fine-grained personal access token** with read-only access. There is no OAuth flow —
you paste a token into the app and it is stored in your macOS Keychain.

## Create a fine-grained PAT

1. Go to **GitHub → Settings → Developer settings → Personal access tokens →
   Fine-grained tokens** (<https://github.com/settings/personal-access-tokens>).
2. Click **Generate new token**.
3. Scope it to the repositories whose CI you want to watch (or the whole org).
4. Under **Repository permissions**, grant **read-only** access to:
   - **Actions** — required (reads workflow runs and self-hosted runner status).
   - **Contents** — required for the Repos panel's remote branch counts.
   - **Issues** — required for the Repos panel's open-issue counts.
   - **Pull requests** — required for the Repos panel's open-PR counts.
   - **Metadata** — read-only (granted automatically alongside other permissions).
5. Leave everything else at **No access** — Solador never writes to GitHub.
6. Generate the token and copy it (it is shown only once).

## Add the token to Solador

1. Open **Settings → GitHub Token**.
2. Paste the token into the **Fine-grained PAT** field and click **Save**.
3. The status shows **Token stored** once it is saved to the Keychain.

The in-app help reads: *"Used by the Repos panel. Grant the fine-grained PAT read
access to Actions (workflow runs), Contents (remote branch counts), Issues (open-issue
counts), and Pull requests (open-PR counts). Stored in your macOS Keychain."* — keep
this doc in sync with that text.

## Troubleshooting

### CI panels show no data / 401
- Confirm the token has **read** access to **Actions** (plus **Contents**, **Issues**,
  and **Pull requests** for the Repos panel's branch/issue/PR counts) for the
  repositories shown. A missing Issues/Pull requests scope shows "—" in those columns
  but does not break the rest of the panel.
- Confirm the token hasn't expired and was scoped to the right repos/org.
- Re-save a fresh token via **Settings → GitHub Token → Clear**, then **Save**.

### Rate limiting
Authenticated requests get 5,000/hour; unauthenticated get 60/hour. If panels stop
updating, you may be rate-limited — wait for the window to reset.
