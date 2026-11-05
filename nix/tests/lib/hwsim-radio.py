"""TEST ONLY: create a mac80211_hwsim radio with a given PERMANENT MAC address (HWSIM_CMD_NEW_RADIO with
HWSIM_ATTR_PERM_ADDR, over generic netlink; standard library only).

Why: NetworkManager matches a profile's 802-11-wireless.mac-address against the device's permanent address, which
`ip link set address` does not change. A Wi-Fi profile pinned to the old machine's card (katana's 5 GHz one is) can
only be brought up in a VM on a radio whose permanent address IS that card's (p2-gates.md H2).

usage: hwsim-radio MAC        prints the new radio's id; its netdev carries MAC as permanent and current address
"""
import os
import socket
import struct
import sys

NETLINK_GENERIC = 16
GENL_ID_CTRL = 0x10
CTRL_CMD_GETFAMILY = 3
CTRL_ATTR_FAMILY_ID = 1
CTRL_ATTR_FAMILY_NAME = 2
NLM_F_REQUEST = 0x1
NLM_F_ACK = 0x4
NLMSG_ERROR = 0x2
# include/uapi/linux/mac80211_hwsim.h (drivers/net/wireless/virtual/mac80211_hwsim.h)
HWSIM_CMD_NEW_RADIO = 4
HWSIM_ATTR_PERM_ADDR = 22


def attr(kind, payload):
    n = 4 + len(payload)
    return struct.pack("HH", n, kind) + payload + b"\0" * ((4 - n % 4) % 4)


def attrs(buf):
    out, i = {}, 0
    while i + 4 <= len(buf):
        n, kind = struct.unpack_from("HH", buf, i)
        if n < 4:
            break
        out[kind & 0x3FFF] = buf[i + 4:i + n]
        i += (n + 3) & ~3
    return out


def request(sock, family, cmd, version, payload, seq):
    body = struct.pack("BBH", cmd, version, 0) + payload
    sock.send(struct.pack("IHHII", 16 + len(body), family, NLM_F_REQUEST | NLM_F_ACK, seq, 0) + body)
    replies = []
    while True:
        data = sock.recv(65536)
        i = 0
        while i + 16 <= len(data):
            n, kind, _flags, rseq, _pid = struct.unpack_from("IHHII", data, i)
            msg = data[i + 16:i + n]
            i += (n + 3) & ~3
            if rseq != seq:
                continue
            if kind == NLMSG_ERROR:
                (err,) = struct.unpack_from("i", msg, 0)
                return err, replies
            replies.append(msg)


def main():
    mac = sys.argv[1] if len(sys.argv) == 2 else sys.exit(__doc__)
    perm = bytes(int(x, 16) for x in mac.split(":"))
    assert len(perm) == 6, mac
    sock = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_GENERIC)
    sock.bind((0, 0))
    err, replies = request(sock, GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 1,
                           attr(CTRL_ATTR_FAMILY_NAME, b"MAC80211_HWSIM\0"), 1)
    if err < 0 or not replies:
        sys.exit(f"hwsim-radio: no MAC80211_HWSIM netlink family ({os.strerror(-err) if err else 'no reply'})")
    (family,) = struct.unpack("H", attrs(replies[0][4:])[CTRL_ATTR_FAMILY_ID][:2])
    err, _ = request(sock, family, HWSIM_CMD_NEW_RADIO, 2, attr(HWSIM_ATTR_PERM_ADDR, perm), 2)
    if err < 0:
        sys.exit(f"hwsim-radio: NEW_RADIO refused: {os.strerror(-err)}")
    print(err)  # the new radio's id (the kernel acknowledges with it)


if __name__ == "__main__":
    main()
