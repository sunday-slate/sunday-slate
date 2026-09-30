{
  description = "Sunday Slate";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" ] (system:
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
          inherit sunday-slate;
          default = sunday-slate;
        };

        apps = rec {
          sunday-slate = flake-utils.lib.mkApp {
            drv = self.packages.${system}.sunday-slate;
          };
          default = sunday-slate;
        };

        checks.module-eval = (nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.default
            {
              system.stateVersion = "26.05";
              services.sunday-slate = {
                enable = true;
                settings = {
                  bind_addr = "0.0.0.0:8080";
                  base_url = "https://slate.example.com";
                  smtp = {
                    host = "smtp.example.com";
                    port = 587;
                    username = "slate";
                  };
                };
                environmentFile = "/run/secrets/sunday-slate";
              };
            }
          ];
        }).config.systemd.units."sunday-slate.service".unit;
      }
    ) // {
      nixosModules.default = import ./nix/module.nix;
    };
}
