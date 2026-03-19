{ lib
, rustPlatform
, pkg-config
, protobuf
, stdenv
}:

rustPlatform.buildRustPackage rec {
  pname = "athenut-mint";
  version = "0.1.0";

  src = lib.cleanSource ../.;

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = {
      "cashu-0.15.1" = "sha256-oAb+okqfcfBpTY8jtd1J0tUopNC2O6Fkq/qcKzS8nCs=";
    };
  };

  nativeBuildInputs = [
    pkg-config
    protobuf
  ];

  env = {
    PROTOC = "${protobuf}/bin/protoc";
    PROTOC_INCLUDE = "${protobuf}/include";
  };

  meta = with lib; {
    description = "Privacy-preserving paid web search API using Cashu ecash";
    homepage = "https://github.com/thesimplekid/athenut-mint";
    license = licenses.unlicense;
    platforms = platforms.linux ++ platforms.darwin;
    mainProgram = "athenut-mint";
  };
}
