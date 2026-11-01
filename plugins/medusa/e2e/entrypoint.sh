#!/bin/sh
set -e
cd /app
npx medusa db:migrate
npx medusa exec ./src/scripts/seed.ts
npx medusa user -e admin@example.com -p e2e-admin-password >/dev/null 2>&1 || true
cd .medusa/server
ln -sf /app/node_modules node_modules
exec npx medusa start
