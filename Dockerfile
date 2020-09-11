FROM ekidd/rust-musl-builder:1.46.0 AS BUILDER

ADD --chown=rust:rust . ./

RUN echo '[source.crates-io]\nreplace-with = "tuna"\n[source.tuna]\nregistry = "https://mirrors.tuna.tsinghua.edu.cn/git/crates.io-index.git"' | sudo tee -a ~/.cargo/config
RUN cargo build --verbose --release

FROM alpine:3.11

COPY --from=builder \
    /home/rust/src/target/x86_64-unknown-linux-musl/release/flv-keyframes \
    /usr/local/bin/
RUN mkdir /share

ENTRYPOINT ["flv-keyframes"]
