{
  lib,
  rustPlatform,
  tailwindcss,
  src,
}:

rustPlatform.buildRustPackage {
  pname = "sunday-slate";
  version = (builtins.fromTOML (builtins.readFile ../crates/sunday-slate/Cargo.toml)).package.version;
  # Keep only build inputs; docs, CI, and the mold linker config must not enter the source.
  src = lib.cleanSourceWith {
    inherit src;
    filter = path: _: lib.any
      (name: path == "${toString src}/${name}" || lib.hasPrefix "${toString src}/${name}/" path)
      [ "Cargo.toml" "Cargo.lock" "crates" ".sqlx" ];
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
