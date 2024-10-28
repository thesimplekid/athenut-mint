{
  description = "CDK Flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };

    flake-utils.url = "github:numtide/flake-utils";

    pre-commit-hooks.url = "github:cachix/pre-commit-hooks.nix";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, pre-commit-hooks, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        lib = pkgs.lib;
        stdenv = pkgs.stdenv;
        isDarwin = stdenv.isDarwin;
        libsDarwin = with pkgs; lib.optionals isDarwin [
          # Additional darwin specific inputs can be set here
          darwin.apple_sdk.frameworks.Security
          darwin.apple_sdk.frameworks.SystemConfiguration
        ];

        # Dependencies
        pkgs = import nixpkgs {
          inherit system overlays;
        };


        # Toolchains
        # latest stable
        stable_toolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "wasm32-unknown-unknown" ]; # wasm
        };

        # Common inputs
        envVars = { };
        buildInputs = with pkgs; [
          # Add additional build inputs here
          git
          pkg-config
          curl
          just
          nixpkgs-fmt
          rust-analyzer
          typos
        ] ++ libsDarwin;

        # WASM deps
        WASMInputs = with pkgs; [
        ];

        nativeBuildInputs = with pkgs; [
          #Add additional build inputs here
        ] ++ lib.optionals isDarwin [
          # Additional darwin specific native inputs can be set here
        ];
      in
      {
        checks = {
          # Pre-commit checks
          pre-commit-check =
            let
              # this is a hack based on https://github.com/cachix/pre-commit-hooks.nix/issues/126
              # we want to use our own rust stuff from oxalica's overlay
              _rust = pkgs.rust-bin.stable.latest.default;
              rust = pkgs.buildEnv {
                name = _rust.name;
                inherit (_rust) meta;
                buildInputs = [ pkgs.makeWrapper ];
                paths = [ _rust ];
                pathsToLink = [ "/" "/bin" ];
                postBuild = ''
                  for i in $out/bin/*; do
                    wrapProgram "$i" --prefix PATH : "$out/bin"
                  done
                '';
              };
            in
            pre-commit-hooks.lib.${system}.run {
              src = ./.;
              hooks = {
                rustfmt = {
                  enable = true;
                  entry = lib.mkForce "${rust}/bin/cargo-fmt fmt --all -- --config format_code_in_doc_comments=true --check --color always";
                };
                nixpkgs-fmt.enable = true;
                typos.enable = true;
                commitizen.enable = true; # conventional commits
              };
            };
        };

        devShells =
          let
            # pre-commit-checks
            _shellHook = (self.checks.${system}.pre-commit-check.shellHook or "");


            stable = pkgs.mkShell ({
              shellHook = "${_shellHook}";
              buildInputs = buildInputs ++ WASMInputs ++ [ stable_toolchain ];
              inherit nativeBuildInputs;
            } // envVars);


          in
          {
            inherit stable;
            default = stable;
          };
      }
    );
}
