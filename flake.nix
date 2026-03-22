{
  description = "Шифр — анонимный обменник файлов — dev shell, builds & container images";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Rust toolchain from rust-toolchain.toml
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common source filtering — only Rust/TOML/IDL files
        src = pkgs.lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type)
            || (builtins.match ".*\\.idl$" path != null);
        };

        # Shared build inputs (native libs needed at compile time)
        nativeBuildInputs = with pkgs; [ pkg-config ];
        buildInputs = with pkgs; [
          openssl
        ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.darwin.apple_sdk.frameworks.Security
          pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
        ];

        commonArgs = {
          inherit src nativeBuildInputs buildInputs;
          strictDeps = true;
        };

        # Build workspace deps once (cache layer)
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Individual service binaries
        mkService = name: craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = name;
          cargoExtraArgs = "--package ${name}";
          doCheck = false; # checks run separately
        });

        gateway  = mkService "gateway";
        signaling = mkService "signaling";
        relay    = mkService "relay";
        load-test = mkService "load-test";

        # Minimal OCI images (no distro, just the binary)
        mkImage = name: bin: pkgs.dockerTools.buildLayeredImage {
          inherit name;
          tag = "latest";
          contents = [ bin pkgs.cacert ];
          config = {
            Entrypoint = [ "/bin/${name}" ];
            ExposedPorts = {
              "9090/tcp" = {}; # prometheus
            };
            Env = [
              "RUST_LOG=info"
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
            ];
          };
        };

      in {
        # ── Packages ──────────────────────────────────────────────────────
        packages = {
          inherit gateway signaling relay load-test;

          docker-gateway   = mkImage "gateway"   gateway;
          docker-signaling = mkImage "signaling" signaling;
          docker-relay     = mkImage "relay"     relay;

          default = pkgs.symlinkJoin {
            name = "cypher-services";
            paths = [ gateway signaling relay ];
          };
        };

        # ── CI Checks ─────────────────────────────────────────────────────
        checks = {
          workspace-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--workspace -- -D warnings";
          });

          workspace-fmt = craneLib.cargoFmt {
            inherit src;
          };

          workspace-test = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            cargoNextestExtraArgs = "--workspace";
          });

          workspace-doc = craneLib.cargoDoc (commonArgs // {
            inherit cargoArtifacts;
            RUSTDOCFLAGS = "-D warnings";
          });
        };

        # ── Dev Shell ─────────────────────────────────────────────────────
        devShells.default = craneLib.devShell {
          checks = self.checks.${system};

          packages = with pkgs; [
            # Rust extras
            cargo-nextest
            cargo-watch
            cargo-fuzz
            cargo-deny
            cargo-audit
            cargo-machete

            # Infrastructure
            docker-compose
            redis
            natscli

            # Tools
            just
            jq
            grpcurl
            hyperfine
          ];

          RUST_LOG = "debug";

          shellHook = ''
            echo "p2p dev shell ready — $(rustc --version)"
            echo "  just infra-up     — start Redis + NATS"
            echo "  just test         — run tests"
            echo "  cargo watch -x check — continuous checking"
          '';
        };

        # ── Runnable apps ─────────────────────────────────────────────────
        apps = {
          gateway = flake-utils.lib.mkApp { drv = gateway; };
          signaling = flake-utils.lib.mkApp { drv = signaling; };
          relay = flake-utils.lib.mkApp { drv = relay; };
          load-test = flake-utils.lib.mkApp { drv = load-test; };
          default = flake-utils.lib.mkApp { drv = gateway; };
        };
      });
}
