FROM alpine:3.11.5 AS builder

# show backtraces
ENV RUST_BACKTRACE 1

RUN apk add --no-cache \
  build-base \
  cargo \
  cmake \
  eudev-dev \
  linux-headers \
  perl \
  clang \
  rust

WORKDIR /parity
COPY . /parity
RUN cargo build --release --target x86_64-alpine-linux-musl --verbose
RUN strip target/x86_64-alpine-linux-musl/release/parity


FROM alpine:3.11.5

RUN apk add --no-cache \
  libstdc++ \
  eudev-libs \
  libgcc

COPY --from=builder /parity/target/x86_64-alpine-linux-musl/release/parity /usr/bin

RUN addgroup -g 1000 parity \
  && adduser -u 1000 -G parity -s /bin/sh -D parity

USER parity

RUN mkdir -p /home/parity/.local/share/io.parity.ethereum/

WORKDIR /home/parity
EXPOSE 5001 8080 8082 8083 8545 8546 8180 30303/tcp 30303/udp

RUN parity --version

ENTRYPOINT ["parity"]
