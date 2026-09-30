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
            moduleResult = case:
              let
                harness = {
                  # Bare evalModules has no NixOS tree; declare the slices
                  # module.nix writes into, loosely typed, so the drain can
                  # read back the service config.
                  options.assertions = lib.mkOption {
                    type = lib.types.listOf lib.types.anything;
                    default = [ ];
                  };
                  options.networking = lib.mkOption {
                    type = lib.types.attrsOf lib.types.anything;
                    default = { };
                  };
                  options.systemd = lib.mkOption {
                    type = lib.types.attrsOf lib.types.anything;
                    default = { };
                  };
                  options.users = lib.mkOption {
                    type = lib.types.attrsOf lib.types.anything;
                    default = { };
                  };
                };
                evaled = lib.evalModules {
                  modules = [
                    harness
                    (import ./nix/module.nix)
                    # deep-merge: case.moduleConf keys must not shadow enable
                    (lib.recursiveUpdate { services.sunday-slate.enable = true; } case.moduleConf)
                  ];
                  specialArgs = { pkgs = pkgs; };
                };
                opts = evaled.config.services.sunday-slate;
                record = {
                  bindAddr = opts.bindAddr;
                  baseUrl = opts.baseUrl;
                  mailFrom = opts.mailFrom;
                  season = opts.season;
                  databaseUrl = opts.databaseUrl;
                  nflDatabaseUrl = opts.nflDatabaseUrl;
                  mediaDir = opts.mediaDir;
                  nflSyncIntervalSecs = opts.nflSyncIntervalSecs;
                  smtp =
                    if opts.smtp == null then null else {
                      host = opts.smtp.host;
                      port = opts.smtp.port;
                      username = opts.smtp.username;
                    };
                };
                json = builtins.toJSON ((import ./nix/settings.nix).mkSettings record);
                svc = evaled.config.systemd.services.sunday-slate;
                execStart = svc.serviceConfig.ExecStart;
                preStart = svc.preStart or "";
                loadCredential = svc.serviceConfig.LoadCredential or [ ];
                contains = txt: fs: lib.all (f: lib.hasInfix f txt) fs;
                nots = txt: fs: lib.all (f: !(lib.hasInfix f txt)) fs;
                assertionFails = if case.assertionFails or false then
                  !lib.all (a: a.assertion) evaled.config.assertions
                else
                  true;
                expKeys = case.settingsKeys or null;
                keysOk =
                  if expKeys == null then true
                  else lib.sort lib.lessThan (builtins.attrNames (builtins.fromJSON json))
                    == lib.sort lib.lessThan expKeys;
                r = {
                  ok = assertionFails && keysOk &&
                    (contains json (case.settingsContain or [ ])) &&
                    (nots json (case.settingsOmit or [ ])) &&
                    (contains execStart (case.execStartContain or [ ])) &&
                    (nots execStart (case.execStartNotContain or [ ])) &&
                    (contains preStart (case.preStartContain or [ ])) &&
                    loadCredential == (case.loadCredential or [ ]);
                  failMsg = "module case '${case.name}' failed: rendered=${json} execStart=${execStart} preStart=${preStart} loadCredential=${builtins.toJSON loadCredential}";
                };
              in
              assert lib.assertMsg r.ok r.failMsg;
              r;
            moduleResults = builtins.map moduleResult (import ./nix/tests/module.nix);
          in
          assert lib.all (r: r.ok) results;
          assert lib.all (r: r.ok) moduleResults;
          pkgs.runCommand "module-settings-test" { } "mkdir -p $out; echo ok > $out/marker";
      };
    }
  );
}
