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
      packages = [
              pkgs.rustc pkgs.cargo pkgs.tmux benchmarkPython
              pkgs.bash pkgs.coreutils pkgs.procps pkgs.git pkgs.util-linux
              pkgs.gnutar pkgs.gzip pkgs.gnused pkgs.gawk pkgs.gnugrep
              pkgs.findutils pkgs.diffutils
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
