#!/usr/bin/bash

# Encrypt the production env file with sops + age.
#
#   bash scripts/encrypt_envs.sh <age-public-key>
#   bash scripts/encrypt_envs.sh age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p
#
# Generate a key pair with: age-keygen -o keys.txt
#
# .prod.env (plaintext, gitignored)  ->  secret.env (encrypted, COMMITTED)
#
# The deploy workflow decrypts secret.env back to .prod.env using the
# AGE_PRIVATE_KEY repo secret. Put the private key from keys.txt there.
#
# The website is a single Rust service, so there is exactly one env file. (The
# earlier version of this script encrypted backend/, frontend/ and an FCM
# service-account JSON — none of which exist in this project.)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."

PUBLIC_KEY="$1"

if [ -z "$PUBLIC_KEY" ]; then
    echo "Usage: bash scripts/encrypt_envs.sh <age-public-key>"
    echo ""
    echo "Example:"
    echo "  bash scripts/encrypt_envs.sh age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p"
    echo ""
    echo "Generate a key pair with: age-keygen -o keys.txt"
    exit 1
fi

if ! command -v sops &> /dev/null; then
    echo "ERROR: sops is not installed."
    echo "Install it: https://github.com/getsops/sops/releases"
    exit 1
fi

PLAINTEXT="$PROJECT_ROOT/.prod.env"
ENCRYPTED="$PROJECT_ROOT/secret.env"

if [ ! -f "$PLAINTEXT" ]; then
    echo "ERROR: .prod.env not found at $PLAINTEXT"
    echo "Create it first (see README -> Environment)."
    exit 1
fi

# sops's dotenv parser dies on blank lines. Comments are fine, so .prod.env uses
# `#` separators — this guard only fires if someone reintroduces a blank line.
if grep -qP '^\s*$' "$PLAINTEXT"; then
    echo "WARN  .prod.env has blank lines - removing them before encryption"
    sed -i '/^\s*$/d' "$PLAINTEXT"
fi

echo "ENCRYPT  .prod.env -> secret.env"
sops --encrypt --age "$PUBLIC_KEY" "$PLAINTEXT" > "$ENCRYPTED"

echo ""
echo "Done. Commit secret.env; never commit .prod.env."
echo "Decrypt locally with: sops --decrypt secret.env > .prod.env"
