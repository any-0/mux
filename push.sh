#!/usr/bin/env bash
set -euo pipefail

nix build .#default --no-link --print-out-paths | cachix push any-0-mux
