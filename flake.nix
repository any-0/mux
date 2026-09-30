{
  description = "mux terminal multiplexer";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      # Use precisely the same nixpkgs/Rust pin as packages.mux, rather than
      # a second rust-overlay or a host toolchain. Plugin sources are pinned
      # (including hashes) by this nixpkgs revision in flake.lock.
      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          benchmarkPython = pkgs.python3.withPackages (p: [ p.pyte ]);
          benchmarkShell = pkgs.writeShellScript "mux-benchmark-shell" ''
            export PS1='BENCH_READY> '
            export HISTFILE=/dev/null
            exec ${pkgs.bash}/bin/bash --noprofile --norc -i
          '';
        in {
          benchmark = pkgs.mkShell {
            packages = [
              pkgs.rustc pkgs.cargo pkgs.tmux benchmarkPython
              pkgs.bash pkgs.coreutils pkgs.procps pkgs.git pkgs.util-linux
              pkgs.tmuxPlugins.resurrect pkgs.tmuxPlugins.continuum
            ];
            BENCH_SHELL = "${benchmarkShell}";
            BENCH_RESURRECT = "${pkgs.tmuxPlugins.resurrect.rtp}";
            BENCH_CONTINUUM = "${pkgs.tmuxPlugins.continuum.rtp}";
            BENCH_NIXPKGS_REV = nixpkgs.rev;
            BENCH_RUST_VERSION = pkgs.rustc.version;
            BENCH_TMUX_VERSION = pkgs.tmux.version;
          };
        });
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          mux = pkgs.rustPlatform.buildRustPackage {
            pname = "mux";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            meta.mainProgram = "mux";
          };
        in
        {
          inherit mux;
          default = mux;
        });
    };
}
