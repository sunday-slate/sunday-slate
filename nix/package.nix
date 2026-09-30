{
  lib,
  rustPlatform,
  tailwindcss,
  src,
}:

rustPlatform.buildRustPackage {
  pname = "sunday-slate";
  version = (builtins.fromTOML (builtins.readFile ../crates/sunday-slate/Cargo.toml)).package.version;
  # The sandbox has no mold linker; exclude the repo's .cargo configuration.
  src = lib.cleanSourceWith {
    inherit src;
    filter = path: _: baseNameOf path != ".cargo";
  };
  cargoLock = {
    lockFile = ../Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  nativeBuildInputs = [ tailwindcss ];
  env = {
    SQLX_OFFLINE = "true";
    TAILWINDCSS = "${tailwindcss}/bin/tailwindcss";
  };
  cargoBuildFlags = [ "--package" "sunday-slate" ];

  # Socket-binding tests cannot run in the Nix sandbox. Run them with `just test`.
  doCheck = false;

  meta = {
    description = "Season-long DFS-style fantasy football, community-owned and privacy-first";
    homepage = "https://github.com/sunday-slate/sunday-slate";
    license = lib.licenses.agpl3Plus;
    mainProgram = "sunday-slate";
    platforms = [ "x86_64-linux" ];
  };
}
