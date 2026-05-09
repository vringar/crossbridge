{ config, lib, pkgs, ... }:

let
  cfg = config.services.crossbridge-supervisor;
in
{
  options.services.crossbridge-supervisor = {
    enable = lib.mkEnableOption "crossbridge per-user supervisor (peer-group socket coordinator)";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ../default.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ../default.nix { }";
      description = "The crossbridge package providing the crossbridge-supervisor binary.";
    };

    socketRoot = lib.mkOption {
      type = lib.types.str;
      default = "%t/crossbridge";
      description = ''
        Runtime directory under which the supervisor binds its register socket
        (`''${socketRoot}/register.socket`) and creates per-peer slug
        subdirectories. The supervisor wipes this directory on startup.

        The default uses the systemd specifier `%t`, which expands to
        `$XDG_RUNTIME_DIR` (typically `/run/user/$UID`) for user units. The
        module also configures `RuntimeDirectory = "crossbridge"` so systemd
        creates and tears the directory down with the unit.

        Exposed to the supervisor via `CROSSBRIDGE_SOCKET_ROOT`. Repo servers
        and clients run by the same user must use the same value to find the
        register socket.
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "crossbridge_supervisor=info";
      description = "RUST_LOG filter string for the supervisor.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.user.services.crossbridge-supervisor = {
      description = "Crossbridge per-user supervisor";
      wantedBy = [ "default.target" ];

      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/crossbridge-supervisor";
        Restart = "on-failure";
        RestartSec = "5s";

        RuntimeDirectory = "crossbridge";
        RuntimeDirectoryMode = "0700";
      };

      environment = {
        RUST_LOG = cfg.logLevel;
        CROSSBRIDGE_SOCKET_ROOT = cfg.socketRoot;
      };
    };
  };
}
