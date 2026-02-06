FROM rust:1.84-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p humansync-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/humansync-server /usr/local/bin/
EXPOSE 4433/udp 8080
VOLUME /data
ENTRYPOINT ["humansync-server"]
CMD ["--data-dir", "/data"]
