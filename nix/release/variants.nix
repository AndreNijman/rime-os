# The toplevels a release publishes (docs/nixos/updates-and-release.md §3.1 `variants`).
#
# `publishable = false` makes `rime-nix-release publish` refuse the variant unless --allow-test-variants is given
# (the stand-in only). rime-vm carries the rime/rime test account and must never reach a real machine.
# TODO(handover): D9's installer configurations, `plain` and `luks`, replace this with publishable = true entries.
{ self }:
{
  plain = {
    system = self.nixosConfigurations.rime-vm;
    publishable = false;
    note = "rime-vm is a test configuration (test account); D9's installer plain/luks configs are not built yet";
  };
}
