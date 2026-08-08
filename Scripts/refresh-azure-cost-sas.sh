#!/usr/bin/env bash
# Re-mints the Azure Cost panel's SAS and updates the Keychain item the app reads.
#
# Why this exists: stsassydog is Entra-only (shared-key auth disabled — it also
# holds Terraform state), so only user-delegation SAS can be minted and those cap
# at 7 days. A permanent SAS is impossible by design; this job is the "permanent"
# alternative. AzureCostService re-reads the Keychain on every refresh cycle, so
# an updated item is picked up without relaunching the app.
#
# Both apps read the per-item entry this script writes: the frozen Swift app
# directly, and the Tauri app because AzureCostSasUrl is exempt from the
# secrets_v1 consolidation (crates/store::secrets — externally-written
# secrets stay per-item; the blob would shadow every re-mint).
#
# Installed as a LaunchAgent (runs every 4 days + at login):
#   ~/Library/LaunchAgents/com.sassydog.devcanopy.azurecost-sas.plist
# Remove with:
#   launchctl bootout "gui/$(id -u)/com.sassydog.devcanopy.azurecost-sas"
#   rm ~/Library/LaunchAgents/com.sassydog.devcanopy.azurecost-sas.plist
#
# Requires: az CLI logged in as an identity with data-plane read + delegation-key
# rights on the container. If az auth lapses, this exits non-zero, the existing
# token keeps working until expiry, and the panel's "SAS expired" state is the
# visible failure signal.
set -euo pipefail

AZ=/opt/homebrew/bin/az
ACCOUNT=stsassydog
CONTAINER=cost-exports
KEYCHAIN_SERVICE=com.sassydog.devcanopy
KEYCHAIN_ACCOUNT=azure_cost_sas_url

EXPIRY=$(date -u -v+7d +%Y-%m-%dT%H:%MZ)
SAS=$("$AZ" storage container generate-sas \
    --account-name "$ACCOUNT" --name "$CONTAINER" \
    --permissions rl --expiry "$EXPIRY" \
    --auth-mode login --as-user --https-only -o tsv)
URL="https://${ACCOUNT}.blob.core.windows.net/${CONTAINER}?${SAS}"

# Verify before touching the Keychain — never replace a working token with junk.
CODE=$(curl -s -o /dev/null -w "%{http_code}" \
    "https://${ACCOUNT}.blob.core.windows.net/${CONTAINER}?restype=container&comp=list&${SAS}")
if [[ "$CODE" != "200" ]]; then
    echo "$(date -u +%FT%TZ) SAS verification failed (HTTP $CODE) — Keychain left untouched" >&2
    exit 1
fi

security add-generic-password -U -s "$KEYCHAIN_SERVICE" -a "$KEYCHAIN_ACCOUNT" -w "$URL"
echo "$(date -u +%FT%TZ) refreshed Azure Cost SAS (expires $EXPIRY)"
