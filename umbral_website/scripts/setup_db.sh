#!/usr/bin/bash

# OPTIONAL: create the umbral_website PostgreSQL database + user on a HOST Postgres.
#
# The Docker deployment does NOT need this — compose runs Postgres in a container
# and creates the database from POSTGRES_* automatically. Use this only against a
# bare-metal Postgres. Reads POSTGRES_* from .prod.env (fallback .env).
#
# Usage: sudo bash scripts/setup_db.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."

ENV_FILE="$PROJECT_ROOT/.prod.env"
[ -f "$ENV_FILE" ] || ENV_FILE="$PROJECT_ROOT/.env"

if [ -f "$ENV_FILE" ]; then
    export $(grep -E '^POSTGRES_(USER|PASSWORD|DB)=' "$ENV_FILE" | xargs)
else
    echo "ERROR: no .prod.env or .env found in $PROJECT_ROOT"
    exit 1
fi

DB_NAME="${POSTGRES_DB:-umbral_website}"
DB_USER="${POSTGRES_USER:-umbral_website}"
DB_PASSWORD="${POSTGRES_PASSWORD:?POSTGRES_PASSWORD must be set in the env file}"

echo "Setting up PostgreSQL database..."
echo "  Database: $DB_NAME"
echo "  User:     $DB_USER"
echo ""

sudo -u postgres psql <<EOF
CREATE DATABASE ${DB_NAME};
CREATE USER ${DB_USER} WITH ENCRYPTED PASSWORD '${DB_PASSWORD}' CREATEDB;
GRANT ALL PRIVILEGES ON DATABASE ${DB_NAME} TO ${DB_USER};
\c ${DB_NAME}
GRANT ALL ON SCHEMA public TO ${DB_USER};
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO ${DB_USER};
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO ${DB_USER};
EOF

echo ""
echo "Done. Database '${DB_NAME}' created for user '${DB_USER}'."
echo "Apply the schema with:  cargo run -- migrate"
