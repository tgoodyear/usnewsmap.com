# The validator container (README.md): `usnm-boinc cloud validatord` behind the Worker in
# worker.mjs. Built with the repository root as the context (wrangler.jsonc image_build_context).

# Dependencies build in their own layer (cargo-chef), as in docker/Dockerfile.api.
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY rust-toolchain.toml ./
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p usnm-boinc --features cloud-validatord --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p usnm-boinc --features cloud-validatord

FROM debian:bookworm-slim AS runtime
# rclone mirrors the gold directory from R2 (entrypoint.sh); the validator reads R2 itself.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates rclone \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /srv/validator validator
COPY --from=build /src/target/release/usnm-boinc /usr/local/bin/usnm-boinc
COPY edge/validator-container/entrypoint.sh /usr/local/bin/entrypoint.sh
USER validator
WORKDIR /srv/validator
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
