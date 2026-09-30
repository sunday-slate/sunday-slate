{
  lib,
  rustPlatform,
  tailwindcss,
  src,
}:
rustPlatform.buildRustPackage {
  pname = "sunday-slate";
  version = "0.1.2"; # keep in sync with crates/sunday-slate/Cargo.toml
  inherit src;
  cargoLock = {
    lockFile = ../Cargo.lock;
    # Git-vendored deps (axum-login, axum-messages fork, sessions store) need
    # pinned source hashes; fed by the fetch errors during the hash cycle.
    outputHashes = {
      "axum-login-0.18.0" = "sha256-CxxVN0uEicYW+mgyqWWw6sn+jXe64xkJZLbFWaXUeKM=";
      "axum-messages-0.8.0" = "sha256-rfPlTS/QhcYZGRdjMUB1Fv2KNLDuFiXgjRbXzRKqM1U=";
      "tower-sessions-sqlx-store-0.15.0" = "sha256-1+lKmPZM33FpYHWjEmNvjyj6f3Vrkz783kUEqzzj5ag=";
    };
  };

# Vendored crate fetching is driven by `cargoLock` (lockFile + outputHashes);
# a separate cargoHash attr is unused by this nixpkgs generation.
  nativeBuildInputs = [ tailwindcss ];
  env = {
    SQLX_OFFLINE = "true";
    TAILWINDCSS = "${tailwindcss}/bin/tailwindcss";
  };
  cargoBuildFlags = [ "--package" "sunday-slate" ];

  # wiremock-based tests bind loopback ports, which the build sandbox forbids
  # (operation not permitted at socket bind). The workspace suite runs via
  # `just test` / CI instead — cargo test stays the repo's own gate.
  doCheck = false;

  meta = {
    description = "Season-long DFS-style fantasy football, community-owned and privacy-first";
    homepage = "https://github.com/sunday-slate/sunday-slate";
    license = lib.licenses.agpl3Plus;
    mainProgram = "sunday-slate";
  };
}
