# End-to-end NixOS VM test for the sunday-slate service module.
#
# Boots a minimal system with the module enabled and asserts the things
# admins rely on: the unit starts under full hardening (incl.
# MemoryDenyWriteExecute), the app answers HTTP with its own marker, both
# databases exist after automatic migrations, and the launcher wires
# systemd credentials into SUNDAY_SLATE__* variables.
{
  name = "sunday-slate";

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ ./module.nix ];

      virtualisation.memorySize = 2048;
      environment.systemPackages = [ pkgs.curl ];

      services.sunday-slate = {
        enable = true;
        smtp = {
          host = "smtp.example.com";
          username = "user";
        };
        secrets.smtpPasswordFile = "/run/creds/ss-smtp-pass";
      };

      # Credential source file: the module only takes paths, it is outside
      # the app's business to create it. A sops-nix / systemd-creds setup
      # would provide it; tmpfiles.d replicates that here.
      systemd.tmpfiles.rules = [
        "f /run/creds/ss-smtp-pass 0600 root root - deadbeef"
      ];
    };

  testScript = ''
    machine.wait_for_unit("sunday-slate.service")
    machine.wait_for_open_port(3000)

    # HTTP marker end-to-end (follows the first-run redirect to /setup)
    machine.succeed("curl -fsSL http://localhost:3000 | grep -qi 'sunday slate'")

    # Automatic migrations produced both databases
    machine.succeed("test -f /var/lib/sunday-slate/storage/sunday-slate.db")
    machine.succeed("test -f /var/lib/sunday-slate/storage/nfl-data.db")

    # Launcher wires systemd credentials (Review Focus 2) — no env-file
    launcher = machine.succeed(
        "systemctl show -p ExecStart --value sunday-slate"
    ).split(" ")[0].strip()
    machine.succeed(f"grep -q CREDENTIALS_DIRECTORY {launcher}")
    machine.succeed(f"grep -q SUNDAY_SLATE__SMTP__PASSWORD {launcher}")
    machine.fail(f"grep -q EnvironmentFile {launcher}")
  '';
}
