# Rime's kernel. Skeleton stub: nixpkgs' latest kernel until the kernel worker lands the
# CachyOS-based build (bootc: kernel/kernel.pin, Containerfile.kernel). Used only when rime.kernel.enable.
{ linuxPackages_latest }:

linuxPackages_latest.kernel
