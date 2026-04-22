FROM rust:slim-trixie@sha256:7d3701660d2aa7101811ba0c54920021452aa60e5bae073b79c2b137a432b2f4 as chef

WORKDIR /src
ENV CARGO_TERM_COLOR=always

RUN cargo install --locked cargo-chef

FROM chef as planner

COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef as builder

ARG PROFILE=release

COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook \
    --locked \
    --bin smart_exporter \
    --profile=${PROFILE} \
    --recipe-path recipe.json

COPY . .
RUN cargo build \
    --frozen \
    --locked \
    --bin smart_exporter \
    --profile=${PROFILE}

FROM debian:trixie@sha256:35b8ff74ead4880f22090b617372daff0ccae742eb5674455d542bef71ef1999 as final

ARG PROFILE=release

LABEL application=smart_exporter \
      maintainer=M3t0r

RUN groupadd --force --gid 1000 smart_exporter
RUN useradd --no-create-home --uid 1000 --gid 1000 smart_exporter

RUN --mount=target=/var/lib/apt/lists,type=cache,sharing=locked \
    --mount=target=/var/cache/apt,type=cache,sharing=locked \
    apt-get update -q; apt-get install -qy smartmontools sudo

COPY --from=builder "/src/target/${PROFILE}/smart_exporter" /usr/bin/smart_exporter

COPY --chmod=440 ./smart_exporter.sudoers /etc/sudoers.d/smart_exporter

WORKDIR /
ENTRYPOINT ["smart_exporter"]
CMD []

EXPOSE 5000
