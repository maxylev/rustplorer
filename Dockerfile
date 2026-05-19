FROM rust:1.91-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/rustplorer /usr/local/bin/rustplorer

EXPOSE 3000

ENTRYPOINT ["rustplorer"]
