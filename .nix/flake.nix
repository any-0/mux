{
  description = "Rust project";

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
  in {
    devShells = forAllSystems (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        benchmark = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc bash coreutils procps git util-linux
            gnutar gzip gnused gawk gnugrep findutils diffutils
            (python3.withPackages (p: [ p.pyte ]))
            tmux tmuxPlugins.resurrect tmuxPlugins.continuum ];
          BENCH_SHELL = "${pkgs.writeShellScript "mux-benchmark-shell" ''
            export PS1='BENCH_READY> '
            export HISTFILE=/dev/null INPUTRC=/dev/null
            unset PROMPT_COMMAND
            if [ "$#" -eq 0 ]; then set -- -i; fi
            exec ${pkgs.bash}/bin/bash --noprofile --norc "$@"
          ''}";
          BENCH_RESURRECT = "${pkgs.tmuxPlugins.resurrect.rtp}";
          BENCH_CONTINUUM = "${pkgs.tmuxPlugins.continuum.rtp}";
          BENCH_NIXPKGS_REV = nixpkgs.rev;
          BENCH_RUST_VERSION = pkgs.rustc.version;
          BENCH_TMUX_VERSION = pkgs.tmux.version;
        };
        stress = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy bash zsh fish vim
            coreutils git util-linux
            (python3.withPackages (p: [ p.pyte ])) ];
        };
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
          ];
        };
      });
  };
}
