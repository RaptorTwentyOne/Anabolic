# Anabolic's MCP server without a window, over standard input and output,
# for MCP directories that try a server in a container. Inside a container
# it sees the container (its processes, its network); to see a machine, run
# Anabolic on it: https://github.com/RaptorTwentyOne/Anabolic
#
#   docker build -t anabolic .
#   docker run -i --rm anabolic              # speaks MCP on stdin/stdout
#   docker run -i --rm anabolic --allow all  # every kind of tool, not only reading
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Thin LTO and parallel code generation: a much quicker build than the
# release's, for a program that answers a few calls.
ENV CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
RUN cargo build --release --locked --bin anabolic

FROM debian:bookworm-slim
COPY --from=build /src/target/release/anabolic /usr/local/bin/anabolic
ENTRYPOINT ["anabolic", "mcp"]
