{ config, lib, pkgs, ... }:

let
  cfg = config.services.athenut-mint;
in
{
  options.services.athenut-mint = {
    enable = lib.mkEnableOption "the Athenut Mint search service";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.athenut-mint;
      defaultText = lib.literalExpression "pkgs.athenut-mint";
      description = "Package providing the athenut-mint binary.";
    };

    configFile = lib.mkOption {
      type = lib.types.path;
      description = ''
        Path to the athenut-mint TOML configuration file.

        This file contains sensitive values (mnemonic, kagi_auth_token,
        wallet seed) and should be managed with a secrets tool such as
        sops-nix or agenix.

        See config.example.toml in the source repository for the format.
      '';
      example = "/run/secrets/athenut-mint.toml";
    };

    workDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/athenut-mint";
      description = "Working directory for database and state files.";
    };

    rustLog = lib.mkOption {
      type = lib.types.str;
      default = "debug,sqlx=warn,hyper=warn,rustls=warn,tungstenite=warn,tokio_tungstenite=warn";
      example = "info,athenut_mint=debug";
      description = ''
        Value for the RUST_LOG environment variable used by the tracing
        subscriber.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.athenut-mint = {
      isSystemUser = true;
      group = "athenut-mint";
      home = cfg.workDir;
      createHome = true;
    };

    users.groups.athenut-mint = { };

    systemd.services.athenut-mint = {
      description = "Athenut Mint - Cashu ecash search service";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      environment = {
        RUST_LOG = cfg.rustLog;
      };

      serviceConfig = {
        Type = "simple";
        User = "athenut-mint";
        Group = "athenut-mint";
        StateDirectory = "athenut-mint";
        WorkingDirectory = cfg.workDir;
        ExecStart = "${cfg.package}/bin/athenut-mint --work-dir ${cfg.workDir} --config ${cfg.configFile}";
        Restart = "on-failure";
        RestartSec = 5;

        # Hardening
        AmbientCapabilities = [ ];
        CapabilityBoundingSet = [ ];
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        PrivateUsers = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        ReadWritePaths = [ cfg.workDir ];
        RemoveIPC = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        UMask = "0077";
      };
    };
  };
}
