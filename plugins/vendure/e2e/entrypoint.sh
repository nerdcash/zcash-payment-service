#!/bin/sh
set -e
cd /app
if [ ! -f /app/.populated ]; then
  node dist/index.js populate
  touch /app/.populated
fi
exec node dist/index.js
