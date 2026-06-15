# Setting up GitHub Integration

DevCanopy's **CI Health** and **CI Runners** panels read GitHub Actions data using a
**fine-grained personal access token** with read-only access. There is no OAuth flow —
you paste a token into the app and it is stored in your macOS Keychain.

## Create a fine-grained PAT

1. Go to **GitHub → Settings → Developer settings → Personal access tokens →
   Fine-grained tokens** (<https://github.com/settings/personal-access-tokens>).
2. Click **Generate new token**.
3. Scope it to the repositories whose CI you want to watch (or the whole org).
4. Under **Repository permissions**, grant **read-only** access to:
   - **Actions** — required (reads workflow runs and self-hosted runner status).
   - **Metadata** — read-only (granted automatically alongside other permissions).
5. Leave everything else at **No access** — DevCanopy never writes to GitHub.
6. Generate the token and copy it (it is shown only once).

## Add the token to DevCanopy

1. Open **Settings → GitHub Token**.
2. Paste the token into the **Fine-grained PAT** field and click **Save**.
3. The status shows **Token stored** once it is saved to the Keychain.

The in-app help reads: *"Used by Portfolio CI to read GitHub Actions runs. A
fine-grained PAT with read access to Actions is sufficient. Stored in your macOS
Keychain."* — keep this doc in sync with that text.

## Troubleshooting

### CI panels show no data / 401
- Confirm the token has **read** access to **Actions** for the repositories shown.
- Confirm the token hasn't expired and was scoped to the right repos/org.
- Re-save a fresh token via **Settings → GitHub Token → Clear**, then **Save**.

### Rate limiting
Authenticated requests get 5,000/hour; unauthenticated get 60/hour. If panels stop
updating, you may be rate-limited — wait for the window to reset.
