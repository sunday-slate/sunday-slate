{
  description = "Sunday Slate";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
  flake-utils.lib.eachDefaultSystem (system:
    let pkgs = nixpkgs.legacyPackages.${system};

    in {
      devShells.default = pkgs.mkShell {
        packages = with pkgs; [
          rustup
          pkg-config
          sqlite
          sqlx-cli
          tailwindcss_4
          just
          bacon
          cargo-deny
          cargo-machete
          cargo-outdated
          cargo-edit
          rust-analyzer
          typos-lsp
          uv # for datasette. replace with datasette pkg and update justfile call when upstream fixed
        ];
      };
    }
  );
}
