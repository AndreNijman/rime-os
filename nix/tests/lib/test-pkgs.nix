# The pkgs nix/tests sees (flake.nix passes this to nix/tests/default.nix): pkgs, except that
# pkgs.testers.runNixOSTest also imports vm-harness.nix into every test. A test added to nix/tests gets the
# harness fixes without opting in.
pkgs:

pkgs
// {
  testers = pkgs.testers // {
    runNixOSTest =
      test:
      pkgs.testers.runNixOSTest {
        imports = [
          (pkgs.lib.setDefaultModuleLocation "a Rime test passed to runNixOSTest" test)
          ./vm-harness.nix
        ];
      };
  };
}
