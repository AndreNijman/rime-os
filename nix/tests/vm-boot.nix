# Boots a machine with nixosModules.default and waits for multi-user.target.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-vm-boot";

  # nixosModules.default sets nixpkgs.overlays (pkgs.rime.*), so nodes evaluate their own nixpkgs.
  node.pkgsReadOnly = false;

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.succeed("test -e /run/current-system")
  '';
}
