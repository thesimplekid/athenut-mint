{
  description = "CDK Flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };

    flake-utils.url = "github:numtide/flake-utils";

  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
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
        stable_toolchain = pkgs.rust-bin.stable.latest.default;

                # Nightly used for formatting
        nightly_toolchain = pkgs.rust-bin.selectLatestNightlyWith (
          toolchain:
          toolchain.default.override {
            extensions = [
              "rustfmt"
              "clippy"
              "rust-analyzer"
              "rust-src"
            ];
            targets = [ "wasm32-unknown-unknown" ]; # wasm
          }
        );


        # Common inputs
        buildInputs = with pkgs; [
          # Add additional build inputs here
          git
          pkg-config
          curl
          just
          nixpkgs-fmt
          rust-analyzer
          typos
          protobuf


        ] ++ libsDarwin;

        # Environment variables
        envVars = {
          PROTOC = "${pkgs.protobuf}/bin/protoc";
          PROTOC_INCLUDE = "${pkgs.protobuf}/include";
        };


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

            nightly = pkgs.mkShell ({
              shellHook = "${_shellHook}";
              buildInputs = buildInputs ++ WASMInputs ++ [ nightly_toolchain ];
              inherit nativeBuildInputs;
            } // envVars);


          in
          {
            inherit stable nightly;
            default = nightly;
          };
      }
    );
}
