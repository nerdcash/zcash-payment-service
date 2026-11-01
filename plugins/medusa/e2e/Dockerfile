# Medusa store with this plugin installed from a local build. Context: plugins/medusa.
FROM node:24-bookworm-slim
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
