{ config, lib, pkgs, ... }:

let
  cfg = config.services.crossbridge;
in
{
  options.services.crossbridge = {
    enable = lib.mkEnableOption "crossbridge cross-project coordination bridge";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ../default.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ../default.nix { }";
      description = "The crossbridge package to use.";
    };

    configFile = lib.mkOption {
      type = lib.types.path;
      description = "Path to the crossbridge.toml configuration file.";
    };

    interval = lib.mkOption {
      type = lib.types.str;
      default = "30s";
      description = "How often to run the crossbridge polling cycle (systemd OnUnitActiveSec format).";
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "crossbridge=info";
      description = "RUST_LOG filter string for crossbridge.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.crossbridge = {
      description = "Crossbridge coordination cycle";
      after = [ "network.target" ];

      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/crossbridge -c ${cfg.configFile}";
        DynamicUser = true;

        # Read access to repo paths referenced in the config.
        # The user must ensure the crossbridge user can read the crosslink
        # database directories (e.g. via group permissions or SupplementaryGroups).
        ReadOnlyPaths = [ cfg.configFile ];

        # Hardening
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        PrivateTmp = true;
        NoNewPrivileges = true;
      };

      environment = {
        RUST_LOG = cfg.logLevel;
      };
    };

    systemd.timers.crossbridge = {
      description = "Run crossbridge on a regular interval";
      wantedBy = [ "timers.target" ];

      timerConfig = {
        OnBootSec = cfg.interval;
        OnUnitActiveSec = cfg.interval;
        Unit = "crossbridge.service";
      };
    };
  };
}
