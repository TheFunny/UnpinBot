FROM rust:1-alpine AS build
WORKDIR /src

# 1. Dependencies first: only the manifests plus a stub main, so the
#    expensive fetch + compile lives in a layer invalidated only by
#    manifest/lock changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release

# 2. Real sources last: only our crate recompiles on source changes. Cargo's
#    freshness check is mtime-based; the COPY'd host files usually predate the
#    stub build, so cargo would consider the stub up to date and never compile
#    the real sources. `touch` makes every .rs newer than the stub artifacts,
#    forcing a rebuild of just this crate while the dependency layer stays
#    cached. (`cargo clean -p` does NOT work here — it removes 0 files and the
#    stub binary silently ships.)
COPY lang/ ./lang/
COPY src/ ./src/
RUN find src -type f -name '*.rs' -exec touch {} + \
    && cargo build --release \
    && cp target/release/unpinbot /unpinbot

FROM alpine:3.20
RUN apk add --no-cache su-exec
COPY --from=build /unpinbot /app/unpinbot
COPY docker-entrypoint.sh /app/
ENTRYPOINT ["/app/docker-entrypoint.sh"]
CMD ["/app/unpinbot"]
