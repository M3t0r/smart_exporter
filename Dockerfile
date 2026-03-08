FROM rust:slim-trixie as chef

WORKDIR /src

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
    --color=always \
    --bin smart_exporter \
    --profile=${PROFILE}

FROM debian:trixie as final

ARG VERSION
ARG TARGETOS TARGETARCH
ARG PROFILE=release

LABEL application=smart_exporter \
      version=${VERSION} \
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
