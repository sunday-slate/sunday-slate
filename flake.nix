{
  description = "Sunday Slate";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
  flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = nixpkgs.legacyPackages.${system};
      sunday-slate = pkgs.callPackage ./nix/package.nix {
        src = self;
        tailwindcss = pkgs.tailwindcss_4;
      };

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
          python3Packages.mkdocs-material # docs previewer/build, see justfile 'docs' recipes
        ];
      };

      packages = {
        sunday-slate = sunday-slate;
        default = sunday-slate;
      };

      apps = {
        sunday-slate = flake-utils.lib.mkApp {
          drv = sunday-slate;
          name = "sunday-slate";
        };
        default = flake-utils.lib.mkApp {
          drv = sunday-slate;
          name = "sunday-slate";
        };
      };

      checks = {
        module-settings =
          let
            lib = nixpkgs.lib;
            cases = import ./nix/tests/settings.nix;
            result = case:
              let
                json = builtins.toJSON ((import ./nix/settings.nix).mkSettings case.input);
                contain = case.contain or [ ];
                omit = case.omit or [ ];
                missing = builtins.filter (f: !(lib.hasInfix f json)) contain;
                leaked = builtins.filter (k: lib.hasInfix ("\"${k}\\\":") json) omit;
                r = {
                  inherit json;
                  ok = missing == [ ] && leaked == [ ];
                  failMsg = "case '${case.name}' failed: missing fragments ${builtins.toJSON missing}, leaked keys ${builtins.toJSON leaked}, rendered=${json}";
                };
              in
              assert lib.assertMsg r.ok r.failMsg;
              r;
            results = builtins.map result cases;
          in
          assert lib.all (r: r.ok) results;
          pkgs.runCommand "module-settings-test" { } "mkdir -p $out; echo ok > $out/marker";
      };
    }
  );
}
