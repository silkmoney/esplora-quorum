{
  description = "Read Bitcoin chain state from several Esplora providers without trusting any one of them";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      treefmt-nix,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      treefmtFor =
        system:
        treefmt-nix.lib.evalModule (pkgsFor system) {
          projectRootFile = "flake.nix";
          programs.nixfmt.enable = true;
          programs.rustfmt.enable = true;
        };

      # The toolchain pinned in rust-toolchain.toml — including the
      # wasm32-unknown-unknown target — so the dev shell and the checks match
      # what cargo/rustup resolve outside Nix.
      rustFor = system: (pkgsFor system).rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

      # Cargo takes CARGO_HOME from the environment and otherwise falls back to
      # $HOME/.cargo. nixpkgs' cargo hook never sets it, and Nix points HOME at
      # /homeless-shelter — which is a real writable path when a build is not
      # sandboxed, and then trips Nix's purity check on the next derivation.
      # Keeping CARGO_HOME inside the build directory avoids that either way.
      cargoHomeInBuildDir = ''
        export CARGO_HOME="$NIX_BUILD_TOP/cargo-home"
        mkdir -p "$CARGO_HOME"
      '';

      withCargoHome =
        platform:
        platform
        // {
          buildRustPackage =
            args:
            platform.buildRustPackage (
              args
              // {
                preBuild = cargoHomeInBuildDir + (args.preBuild or "");
              }
            );
        };

      rustPlatformFor =
        system:
        let
          pkgs = pkgsFor system;
          rust = rustFor system;
        in
        withCargoHome (
          pkgs.makeRustPlatform {
            cargo = rust;
            rustc = rust;
          }
        );

      # Everything cargo needs and nothing it does not: no flake.nix, no
      # .github/, no deny.toml, so editing those never rebuilds the crate.
      src = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./rust-toolchain.toml
          ./rustfmt.toml
          ./src
          ./tests
          # Named by Cargo.toml, so `cargo package` wants them present.
          ./README.md
          ./CHANGELOG.md
          ./LICENSE-MIT
          ./LICENSE-APACHE
          ./NOTICE
        ];
      };

      version = "0.1.0";

      # A cargo invocation as a check derivation: no artifacts, just a pass or
      # a fail. `--offline` because the vendored registry is all there is.
      #
      # No extra native inputs: the `client` feature's TLS is rustls over ring,
      # which ships pre-generated assembly, and webpki-roots bundles the trust
      # store — so nothing here needs cmake, perl or the sandbox's /etc/ssl.
      cargoCheck =
        {
          system,
          name,
          command,
        }:
        (rustPlatformFor system).buildRustPackage {
          pname = "esplora-quorum-${name}";
          inherit version src;
          cargoLock.lockFile = ./Cargo.lock;
          # Nothing is installed; the command below is the whole point.
          dontBuild = true;
          doCheck = true;
          preCheck = cargoHomeInBuildDir;
          checkPhase = ''
            runHook preCheck
            ${command}
            runHook postCheck
          '';
          installPhase = "touch $out";
        };
    in
    {
      # No packages output: this crate is a library with no binary, and
      # downstream Rust code depends on it through cargo, not through Nix.
      # `nix flake check` is the entry point.

      checks = forAllSystems (system: {
        # The whole suite, including the `client` feature. Nothing here reaches
        # the network — the client's tests exercise construction and policy.
        tests = cargoCheck {
          inherit system;
          name = "tests";
          command = "cargo test --all-features --offline";
        };

        # The core must keep passing with no features at all — that is the
        # configuration that has to stay I/O-free.
        tests-no-default = cargoCheck {
          inherit system;
          name = "tests-no-default";
          command = "cargo test --no-default-features --offline";
        };

        clippy = cargoCheck {
          inherit system;
          name = "clippy";
          command = "cargo clippy --all-targets --all-features --offline -- -D warnings";
        };

        clippy-no-default = cargoCheck {
          inherit system;
          name = "clippy-no-default";
          command = "cargo clippy --all-targets --no-default-features --offline -- -D warnings";
        };

        # The property that makes this crate usable in a browser, and the one
        # easiest to lose by accident: without the `client` feature the crate
        # must not reach any I/O.
        wasm = cargoCheck {
          inherit system;
          name = "wasm";
          command = ''
            cargo build --target wasm32-unknown-unknown --no-default-features --offline
            cargo build --target wasm32-unknown-unknown --offline
          '';
        };

        # `no_std` for real. wasm32-unknown-unknown HAS a std implementation,
        # so it compiles happily even if the crate has started linking std —
        # it proves the code is portable, not that it is no_std. A bare-metal
        # target has no std at all, so this is the check that can actually
        # fail when someone reaches for `std::` out of habit.
        no-std = cargoCheck {
          inherit system;
          name = "no-std";
          command = "cargo build --target thumbv7em-none-eabi --no-default-features --offline";
        };

        docs = cargoCheck {
          inherit system;
          name = "docs";
          command = ''
            RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps --offline
          '';
        };

        formatting = (treefmtFor system).config.build.check self;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          rust = rustFor system;
          treefmt = (treefmtFor system).config.build.wrapper;

          # Fails the commit if anything is unformatted. treefmt reformats in
          # place, so the fix is `git add` and commit again.
          preCommitHook = pkgs.writeShellScript "pre-commit" ''
            if ! ${treefmt}/bin/treefmt --fail-on-change --no-cache; then
              echo "✖ pre-commit: files were reformatted — 'git add' them and commit again." >&2
              exit 1
            fi
          '';

          # The local gate. Runs incrementally on the host, so it is seconds
          # warm; `FULL=1 git push` runs the sandboxed `nix flake check` too.
          prePushHook = pkgs.writeShellScript "pre-push" ''
            set -e
            export PATH="${rust}/bin:$PATH"
            cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel)"
            echo "▸ pre-push gate — FULL=1 git push runs nix flake check as well"
            ${treefmt}/bin/treefmt --fail-on-change --no-cache
            echo "▸ cargo test (all features, then no features)"
            cargo test --all-features --quiet
            cargo test --no-default-features --quiet
            echo "▸ cargo clippy"
            cargo clippy --all-targets --all-features -- -D warnings
            cargo clippy --all-targets --no-default-features -- -D warnings
            echo "▸ wasm32 build"
            cargo build --target wasm32-unknown-unknown --no-default-features --quiet
            if [ "''${FULL:-0}" = "1" ]; then
              echo "▸ FULL=1: nix flake check"
              nix flake check
            fi
          '';
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              treefmt
              pkgs.cargo-deny
              pkgs.jq
            ];

            shellHook = ''
              echo "rust: $(rustc --version)"
              echo
              echo "  cargo test --all-features        the whole suite"
              echo "  nix flake check                  the same, sandboxed, plus wasm and docs"
              echo "  cargo deny --all-features check  licences, advisories, sources"
              echo
              # Installed from wherever the shell was entered, and never over
              # a contributor's own hook — only over a symlink we placed
              # before, which is what an ordinary toolchain bump looks like.
              hooks="$(${pkgs.git}/bin/git rev-parse --git-path hooks 2>/dev/null || true)"
              if [ -n "$hooks" ] && [ -d "$hooks" ]; then
                for hook in pre-commit:${preCommitHook} pre-push:${prePushHook}; do
                  dest="$hooks/''${hook%%:*}"
                  if [ -e "$dest" ] && [ ! -L "$dest" ]; then
                    echo "note: $dest exists and is not ours — leaving it alone" >&2
                  else
                    ln -sf "''${hook#*:}" "$dest"
                  fi
                done
              fi
            '';
          };
        }
      );

      formatter = forAllSystems (system: (treefmtFor system).config.build.wrapper);
    };
}
