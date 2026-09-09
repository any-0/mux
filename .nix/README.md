# Dev environment

Instead of setting up development tools manually, [Nix](https://nixos.org/) builds the environment described in `.nix/flake.nix`. `.nix/flake.lock` records the versions to use. With Nix installed, recreating that exact environment on Linux, macOS, or Windows (through WSL) is trivial.

[direnv](https://direnv.net/) is set up to load that environment automatically when you `cd` into this directory.

- To add a package or change the toolchain, edit `.nix/flake.nix`, then run `direnv reload`.
- `.nix/flake.lock` pins the exact versions in use. Regenerate it with `nix flake update` from inside `.nix/` if you need newer ones.

Generated from https://g.any-0.com/nix.git, template `rust`, 2026-08-04.
