{
  description = "VOIE Cloud Release 0 development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/9f78f44a87948854445dae0b6bf82b2e87e4efb5";
  inputs.disko.url = "github:nix-community/disko";
  inputs.disko.inputs.nixpkgs.follows = "nixpkgs";

  outputs =
    {
      nixpkgs,
      disko,
      ...
    }:
    let
      system = "x86_64-linux";
      overlay = import ./nix/overlay.nix { repo = ./.; };
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ overlay ];
      };
      nixos =
        modules:
        nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [ { nixpkgs.overlays = [ overlay ]; } ] ++ modules;
        };
      kataModule = import ./nix/runtime/kata-runtime-rs.nix;
      kataRuntimeRs =
        ((import nixpkgs { inherit system; }).nixos {
          imports = [ kataModule ];
        }).config.voie.kataRuntimeRs;
    in
    {
      overlays.default = overlay;

      nixosConfigurations.control = nixos [ ./nix/hosts/control.nix ];
      nixosConfigurations.control-azure-image = nixos [
        ./nix/hosts/control.nix
        ./nix/hosts/control-azure-image.nix
      ];
      nixosConfigurations.fabric = nixos [ ./nix/hosts/fabric.nix ];
      nixosConfigurations.fabric-dev = nixos [ ./nix/hosts/fabric-dev.nix ];

      nixosModules.voie-kata-runtime-rs = kataModule;
      nixosModules.disko = disko.nixosModules.disko;

      packages.${system} = {
        default = pkgs.voie-cloud;
        voie-cloud = pkgs.voie-cloud;
        voie-web = pkgs.voie-web;
        # Prebuilt activation child entry; deployment pins it through
        # VOIE_ACTIVATION_ENTRY in the control service unit.
        voie-activation-dist = pkgs.voie-activation-dist;
        # FD-only privileged handoff pair enforcing the voie-activation UID
        # boundary for activation children on the control host.
        voie-activation-handoff = pkgs.voie-activation-handoff;
        voie-kata-assets = kataRuntimeRs.assets;
        voie-kata-runtime-rs-shim = kataRuntimeRs.patchedShim;
        voie-runner-image = pkgs.callPackage ./nix/runtime/voie-runner-image.nix { };
        voie-workspace-image = pkgs.callPackage ./nix/runtime/voie-workspace-image.nix { };
        voie-app-image = pkgs.callPackage ./nix/runtime/voie-app-image.nix { };
        voie-postgres-image = pkgs.callPackage ./nix/runtime/voie-postgres-image.nix { };
        voie-gateway-image = pkgs.callPackage ./nix/runtime/voie-gateway-image.nix { };
        voie-pause-image = pkgs.callPackage ./nix/runtime/voie-pause-image.nix { };
        voie-c1-pod-manifest = pkgs.callPackage ./nix/runtime/voie-c1-pod.nix { };
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          ansible
          azurite
          caddy
          cargo
          curl
          git
          jq
          just
          nixos-anywhere
          nodejs_22
          opentofu
          openssl
          pnpm
          postgresql_17
          python3
          rustc
          rustfmt
          typescript
          chrome-headless-shell
        ];
      };
    };
}
