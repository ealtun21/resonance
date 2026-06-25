#!/usr/bin/env bash
# Create a stable, self-signed code-signing identity for locally-built
# Resonance.app bundles.
#
# Why: an ad-hoc signature ("codesign -s -") gives the bundle a *cdhash*-based
# designated requirement, so every rebuild changes the cdhash → macOS treats it
# as a brand-new app and re-prompts for the Audio Capture / Screen Recording
# TCC permission. Signing with a persistent identity instead makes the
# designated requirement key on the certificate ("certificate leaf = H…"),
# which is stable across rebuilds — so the TCC grant survives every rebuild
# after the first.
#
# This is a *local development* identity: self-signed, not trusted by
# Gatekeeper, fine for locally-built apps (which aren't quarantined). For
# distribution use a real "Developer ID Application" identity instead.
#
# Idempotent: re-running detects an existing identity and does nothing.
#
# Usage:
#   contrib/macos/make-signing-cert.sh
# then build the app signed with it:
#   SIGN_IDENTITY="Resonance Local Signing" contrib/macos/build-app.sh
set -euo pipefail

IDENTITY="${IDENTITY:-Resonance Local Signing}"
# A dedicated keychain (rather than login.keychain) so this works over SSH —
# the login keychain is locked outside the user's GUI session, but we own this
# one's password and can unlock it non-interactively.
KEYCHAIN="${KEYCHAIN:-$HOME/Library/Keychains/resonance-signing.keychain-db}"
KEYCHAIN_PW="${KEYCHAIN_PW:-resonance}"

if security find-identity -p codesigning 2>/dev/null | grep -qF "$IDENTITY"; then
    echo ">> identity '$IDENTITY' already present — nothing to do"
    security find-identity -p codesigning | grep -F "$IDENTITY"
    exit 0
fi

echo ">> generating self-signed code-signing certificate '$IDENTITY'"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/cs.cnf" <<EOF
[ req ]
distinguished_name = dn
x509_extensions    = ext
prompt             = no
[ dn ]
CN = $IDENTITY
[ ext ]
keyUsage         = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
basicConstraints = critical, CA:false
EOF
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$TMP/cs.key" -out "$TMP/cs.crt" -config "$TMP/cs.cnf" >/dev/null 2>&1
openssl pkcs12 -export -inkey "$TMP/cs.key" -in "$TMP/cs.crt" \
    -out "$TMP/cs.p12" -name "$IDENTITY" -passout "pass:$KEYCHAIN_PW" >/dev/null 2>&1

echo ">> creating dedicated signing keychain $KEYCHAIN"
if [[ ! -f "$KEYCHAIN" ]]; then
    security create-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
fi
security set-keychain-settings "$KEYCHAIN"            # no auto-lock timeout
security unlock-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"

echo ">> importing identity (codesign may use it without prompting)"
security import "$TMP/cs.p12" -k "$KEYCHAIN" -P "$KEYCHAIN_PW" \
    -T /usr/bin/codesign -A
# Add to the user's keychain search list so codesign finds the identity.
EXISTING="$(security list-keychains -d user | sed 's/[[:space:]]*"//; s/"$//')"
# shellcheck disable=SC2086
security list-keychains -d user -s "$KEYCHAIN" $EXISTING
# Let codesign access the private key without the interactive auth prompt.
security set-key-partition-list -S apple-tool:,apple: -s -k "$KEYCHAIN_PW" "$KEYCHAIN" >/dev/null

echo
echo ">> done. Identity available for codesign:"
security find-identity -p codesigning | grep -F "$IDENTITY" || {
    echo "ERROR: identity not visible to codesign" >&2; exit 1; }
echo
echo "Build a signed bundle with:"
echo "    SIGN_IDENTITY=\"$IDENTITY\" contrib/macos/build-app.sh"
