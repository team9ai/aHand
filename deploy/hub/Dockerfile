# syntax=docker/dockerfile:1.7

ARG NODE_VERSION=20-bookworm-slim
ARG RUST_VERSION=1-bookworm

FROM rust:${RUST_VERSION} AS hub-build
WORKDIR /repo

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock Cross.toml ./
COPY crates crates
COPY proto proto

RUN cargo build --release -p ahand-hub

FROM debian:bookworm-slim AS hub
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=hub-build /repo/target/release/ahand-hub /usr/local/bin/ahand-hub

EXPOSE 1515
ENTRYPOINT ["/usr/local/bin/ahand-hub"]

FROM node:${NODE_VERSION} AS dashboard-build
WORKDIR /repo

RUN corepack enable

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml turbo.json tsconfig.base.json ./
COPY apps apps
COPY packages packages
COPY crates crates
COPY proto proto

RUN pnpm install --frozen-lockfile
# hub-dashboard imports @ahandai/proto, which exposes its types via dist/.
# Build proto-ts first so the dashboard's next build can resolve it.
RUN pnpm --filter @ahandai/proto build
RUN pnpm --filter @ahand/hub-dashboard build

FROM node:${NODE_VERSION} AS dashboard
WORKDIR /app

ENV NODE_ENV=production
# `pnpm start` runs `next start -p 1516`. Keep the EXPOSE/PORT
# aligned with that — the smoke tests and compose file map an
# external port to this internal one.
ENV PORT=1516

RUN corepack enable

COPY --from=dashboard-build /repo/package.json /app/package.json
COPY --from=dashboard-build /repo/pnpm-lock.yaml /app/pnpm-lock.yaml
COPY --from=dashboard-build /repo/pnpm-workspace.yaml /app/pnpm-workspace.yaml
COPY --from=dashboard-build /repo/turbo.json /app/turbo.json
COPY --from=dashboard-build /repo/tsconfig.base.json /app/tsconfig.base.json
COPY --from=dashboard-build /repo/node_modules /app/node_modules
COPY --from=dashboard-build /repo/apps/hub-dashboard /app/apps/hub-dashboard

WORKDIR /app/apps/hub-dashboard
EXPOSE 1516

CMD ["pnpm", "start"]
