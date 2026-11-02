# D6, the client side (docs/nixos/updates-and-release.md §6.2, §8.1, §9; docs/nixos/update-client.md).
#
# `rime update` on NixOS reads everything it trusts from /etc/rime/update.toml, written here: the registry and
# channel, the binary cache, the Nix release signing identity, and the store paths of nix, nix-env and nix-store it
# runs. Nothing it trusts comes from the network or from the user's command line. A VM test overrides the trust
# options with a throwaway CA, identity and cache key (nix/tests/update-client.nix) through this same path.
{ config, lib, pkgs, ... }:

let
  cfg = config.rime.update;
  toml = pkgs.formats.toml { };
  nix = config.nix.package;
  floors = import ../release/floors.nix;
  settings = {
    repository = cfg.repository;
    channel = cfg.channel;
    variant = cfg.variant;
    cache_url = cfg.cacheUrl;
    cache_keys = cfg.cacheKeys;
    upstream_cache_keys = cfg.upstreamCacheKeys;
    signers = cfg.signers;
    issuer = cfg.issuer;
    nix = "${nix}/bin/nix";
    nix_env = "${nix}/bin/nix-env";
    nix_store = "${nix}/bin/nix-store";
    keep_generations = cfg.keepGenerations;
    # §11.5: the free-space gate counts on-disk bytes on a compress-force=zstd store (nix/release/floors.nix).
    on_disk_ratio = floors.ratio;
    # Deltas (docs/nixos/delta.md §7): the decoder and the downloader by store path, and where the delta files are.
    deltas = cfg.deltas.enable;
    zstd = "${pkgs.zstd}/bin/zstd";
    curl = "${pkgs.curl}/bin/curl";
  } // lib.optionalAttrs (cfg.fulcioRoot != null) { fulcio_root = "${cfg.fulcioRoot}"; }
    // lib.optionalAttrs (cfg.upstreamCacheUrl != null) { upstream_cache_url = cfg.upstreamCacheUrl; }
    // lib.optionalAttrs (cfg.deltas.url != null) { delta_url = cfg.deltas.url; }
    // lib.optionalAttrs (cfg.deltas.windowLogMax != null) { delta_window_log_max = cfg.deltas.windowLogMax; }
    // lib.optionalAttrs (cfg.deltas.jobs != null) { delta_jobs = cfg.deltas.jobs; };
in
{
  options.rime.update = {
    enable = lib.mkEnableOption "the NixOS backend of `rime update`" // { default = true; };

    repository = lib.mkOption {
      type = lib.types.str;
      default = "ghcr.io/andrenijman/rime-os";
      description = "Where the signed channel document (`nix-channels`) and the release manifests are published (§3.3).";
    };
    channel = lib.mkOption {
      type = lib.types.str;
      default = "edge";
      description = "The default channel; `rime channel set` writes /var/lib/rime/nix/channel over it (§12).";
    };
    variant = lib.mkOption {
      type = lib.types.str;
      default = "plain";
      description = ''
        The base of the release variant this machine installs (§3.1 `variants`, §11.6): `plain` is the generic
        installed system (plain and LUKS alike, D9). The client adds every feature switched on for this machine
        (/var/lib/rime/switch), so a machine with gaming switched on installs `plain-gaming`.
      '';
    };
    cacheUrl = lib.mkOption {
      type = lib.types.str;
      default = "https://cache.rimeos.com";
      description = "Rime's binary cache (§6, §18.1): the Rime-built paths of a release (all of them on the S3 fallback).";
    };
    cacheKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      # The public half nix-release.yml checks RIME_NIX_CACHE_KEY against (§6.2, §18.1).
      default = [ "rime-cache-1:LXocssi9/mdVEgEUlO+thxyiumct4b+HfqW2nIFmX1k=" ];
      description = ''
        Narinfo public keys compiled into the system. A normal update checks downloads against the keys the VERIFIED
        manifest announces; these are used only under `--allow-unverified` (§6.4).
      '';
    };
    upstreamCacheUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "https://cache.nixos.org";
      description = ''
        The upstream binary cache: a release published with the GitHub backend (§18.1) holds only Rime-built paths and
        marks every other path `origin: upstream`, downloaded from here. null refuses such a release.
      '';
    };
    upstreamCacheKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=" ];
      description = "The upstream cache's narinfo keys compiled into the system; like cacheKeys, used only under `--allow-unverified`.";
    };
    signers = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "https://github.com/AndreNijman/rime-os/.github/workflows/nix-release.yml@refs/heads/main" ];
      description = "Keyless (Fulcio) identities accepted on the channel document and release manifests (§5.1).";
    };
    issuer = lib.mkOption {
      type = lib.types.str;
      default = "https://token.actions.githubusercontent.com";
      description = "The OIDC issuer the signing certificate must name.";
    };
    fulcioRoot = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "The CA signing certificates must chain to; null = the Sigstore root pinned in rimed.";
    };
    deltas = {
      enable = lib.mkEnableOption "delta downloads (docs/nixos/delta.md §7): a changed path is rebuilt from a zstd patch against the version this machine has" // { default = true; };
      url = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = "https://github.com/AndreNijman/rime-cache/releases/download";
        description = ''
          Where the delta files of a release are: the manifest names each as `<release tag>/<file>`, relative to this.
          The manifest's signature covers each file's sha256 and size; the URL itself comes only from here (§8.1).
          null turns deltas off.
        '';
      };
      windowLogMax = lib.mkOption {
        type = lib.types.nullOr (lib.types.ints.between 20 31);
        default = null;
        description = ''
          The largest zstd decoder window (2^n bytes of memory per delta being rebuilt) this machine accepts; a delta
          that needs more is skipped and its path downloaded whole. null: 29 (512 MiB), 30 with 8 GiB of memory or more.
        '';
      };
      jobs = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = "Deltas rebuilt at once; null: half the CPUs, between 1 and 6.";
      };
    };
    keepGenerations = lib.mkOption {
      type = lib.types.ints.between 2 50;
      default = 3;
      description = "`rime gc` keeps this many newest system generations, plus the booted, previous and pinned ones (§11.3).";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.etc."rime/update.toml".source = toml.generate "rime-update.toml" settings;

    # skopeo fetches the signed documents; it needs /etc/containers/policy.json and registries.conf.
    environment.systemPackages = [ pkgs.skopeo ];
    virtualisation.containers.enable = lib.mkDefault true;

    # §6.2: the Rime cache and the upstream cache are the only substituters, a store path is accepted only with a
    # valid signature, and a missing path is an error, never a local build.
    nix.settings = {
      substituters = lib.mkForce ([ cfg.cacheUrl ] ++ lib.optional (cfg.upstreamCacheUrl != null) cfg.upstreamCacheUrl);
      trusted-public-keys = lib.mkForce (cfg.cacheKeys ++ lib.optionals (cfg.upstreamCacheUrl != null) cfg.upstreamCacheKeys);
      require-sigs = true;
      fallback = false;
      builders = lib.mkForce "";
    };
    # Generations are removed only by `rime gc`, by number, never past the booted or pinned ones.
    nix.gc.automatic = lib.mkForce false;
    nix.optimise.automatic = lib.mkDefault false;

    # §9: a live switch never restarts the display manager (a restart ends every session). nixpkgs' greetd module
    # sets this too; stated here so the guarantee does not rest on an upstream default (VM-tested).
    systemd.services.greetd.restartIfChanged = false;

    # The update notice (bootc's rime-update-check, D6 §8.3): `rime update --check` verifies the channel document and
    # the manifest it names, downloads nothing, and as root writes /run/rime-update/state (`state=available|none`,
    # `digest=…`), which Rime Shell's Settings -> Updates reads.
    systemd.services.rime-update-check = {
      description = "Check whether a Rime OS update is available";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${config.rime.services.rimed.package}/bin/rime update --check";
        Nice = 19;
        CPUSchedulingPolicy = "idle";
        IOSchedulingClass = "idle";
        RuntimeDirectory = "rime-update";
        RuntimeDirectoryMode = "0755";
        RuntimeDirectoryPreserve = "yes";
        TimeoutStartSec = "2min";
      };
    };
    systemd.timers.rime-update-check = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitActiveSec = "6h";
        RandomizedDelaySec = "20min";
        AccuracySec = "5min";
      };
    };

    # §11.3: the daily gc.
    systemd.services.rime-gc = {
      description = "Remove old Rime system generations";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${config.rime.services.rimed.package}/bin/rime gc";
      };
    };
    systemd.timers.rime-gc = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = "daily";
        RandomizedDelaySec = "1h";
        Persistent = true;
      };
    };
  };
}
