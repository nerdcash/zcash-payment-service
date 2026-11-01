# Medusa store with this plugin installed from a local build. Context: plugins/medusa.
FROM node:26-bookworm-slim@sha256:bc0a2afd3b74ce12222fc8bab99271e66257f021078f5f4a5d687a77d280c1d9
WORKDIR /plugin
COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY tsconfig.json ./
COPY src ./src
RUN npx medusa plugin:build && npm pack --pack-destination /tmp && mv /tmp/nerdcash-medusa-payment-zcash-*.tgz /tmp/plugin.tgz

WORKDIR /app
COPY e2e/app/package.json e2e/app/package-lock.json ./
RUN npm ci --no-audit --no-fund && npm install --no-save --no-audit --no-fund /tmp/plugin.tgz
COPY e2e/app ./
RUN npx medusa build
EXPOSE 9000
COPY e2e/entrypoint.sh /entrypoint.sh
CMD ["/entrypoint.sh"]
