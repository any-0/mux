# Linux run of mux's own tests and the black-box harness: the same pinned
# toolchain as Dockerfile.test, plus what the harness's panes use.
FROM rust:1.90-bookworm

RUN rustup component add rustfmt clippy
RUN apt-get update \
    && apt-get install --yes --no-install-recommends zsh locales procps ncurses-bin vim less nano dialog htop tmux mc \
    && sed -i 's/# en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen \
    && rm -rf /var/lib/apt/lists/*
ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8

WORKDIR /workspace
