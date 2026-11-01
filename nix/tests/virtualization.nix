# The virtual machines feature (nix/modules/virtualization.nix): with rime.virtualization.enable the system has a
# libvirtd that answers, QEMU with KVM support, UEFI firmware for guests, swtpm and virtiofsd, and the catalogue
# reports the feature as in this system. No guest is booted (nested KVM is not assumed on the test host).
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "rime-virtualization";
  node.pkgsReadOnly = false;

  nodes.machine = { config, ... }: {
    imports = [ self.nixosModules.default ];
    rime.catalogue.enable = true;
    rime.virtualization.enable = true;
    environment.systemPackages = [ config.rime.services.rimed.package ];
    virtualisation.memorySize = 2048;
  };

  testScript = ''
    import json

    machine.wait_for_unit("multi-user.target")

    with subtest("the catalogue says this system carries virtual machines"):
        cat = json.loads(machine.succeed("cat /etc/rime/catalogue.json"))
        f = cat["features"]["virtualization"]
        assert f["offered"] and f["in_system"], f
        out = machine.succeed("rime resolve qemu-kvm 2>&1")
        assert "this system carries it" in out, out

    with subtest("libvirtd answers on the system socket, with QEMU as its hypervisor"):
        machine.succeed("systemctl start libvirtd.service")
        machine.wait_for_unit("libvirtd.service")
        print(machine.succeed("virsh -c qemu:///system version"))
        caps = machine.succeed("virsh -c qemu:///system capabilities")
        assert "<arch>x86_64</arch>" in caps, caps
        doms = machine.succeed("virsh -c qemu:///system domcapabilities --machine q35 --arch x86_64 --virttype qemu")
        # UEFI firmware a guest can boot (OVMF, with a Secure Boot build among them), and the TPM emulator.
        assert "<loader supported='yes'>" in doms and "OVMF" in doms, doms
        assert "<tpm supported='yes'>" in doms and "emulator" in doms, doms

    with subtest("the tools rime vm runs directly"):
        machine.succeed("qemu-system-x86_64 --version")
        machine.succeed("swtpm --version")
        machine.succeed("test -x \"$(command -v virtiofsd)\"")
  '';
}
