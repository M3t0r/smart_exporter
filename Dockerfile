FROM rust:slim-bookworm as builder

ARG VERSION
ARG TARGETOS TARGETARCH

# could be "dev" for debug builds
ARG PROFILE=release

WORKDIR /src

COPY Cargo.toml Cargo.lock ./

# pre-compile dependencies
RUN mkdir -p src && \
    echo 'fn main() {println!("wrong main!");}' > src/main.rs && \
    cargo build --profile=${PROFILE}

COPY ./src/ ./src/
RUN touch ./src/main.rs # tell cargo that the binary is outdated
RUN cargo build --frozen --color=always --profile=${PROFILE}

RUN mv ./target/*/smart_exporter ./

FROM debian:bookworm as final

ARG VERSION
ARG TARGETOS TARGETARCH

LABEL application=smart_exporter \
      version=${VERSION} \
      maintainer=M3t0r

RUN groupadd --force --gid 1000 smart_exporter
RUN useradd --no-create-home --uid 1000 --gid 1000 smart_exporter

RUN --mount=target=/var/lib/apt/lists,type=cache,sharing=locked \
    --mount=target=/var/cache/apt,type=cache,sharing=locked \
    apt-get update; apt-get install -y smartmontools sudo

COPY --from=builder "/src/smart_exporter" /usr/bin/smart_exporter

COPY ./smart_exporter.sudoers /etc/sudoers.d/

WORKDIR /
ENTRYPOINT ["smart_exporter"]
CMD []

EXPOSE 5000
