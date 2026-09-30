{
  description = "Sunday Slate";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      sunday-slate = pkgs.callPackage ./nix/package.nix {
        src = self;
        tailwindcss = pkgs.tailwindcss_4;
      };
    in {
      devShells = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ] (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
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
        }
      );

      packages.x86_64-linux = {
        inherit sunday-slate;
        default = sunday-slate;
      };

      nixosModules.default = import ./nix/module.nix;

      checks.x86_64-linux.service-unit = (nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          self.nixosModules.default
          {
            system.stateVersion = "26.05";
            services.sunday-slate.enable = true;
          }
        ];
      }).config.systemd.units."sunday-slate.service".unit;
    };
}
