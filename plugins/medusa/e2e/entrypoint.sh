#!/bin/sh
set -e
cd /app
# On a cold CI runner the first migration can hit Medusa's 10 s connection timeout; migrations
# are idempotent, so retry.
for i in 1 2 3 4 5; do
  npx medusa db:migrate && break
  [ "$i" = 5 ] && exit 1
  echo "db:migrate failed, retrying ($i)"
  sleep 3
done
npx medusa exec ./src/scripts/seed.ts
npx medusa user -e admin@example.com -p e2e-admin-password >/dev/null 2>&1 || true
cd .medusa/server
ln -sf /app/node_modules node_modules
exec npx medusa start
