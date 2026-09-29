# Networking

Goal: firmware that needs the network — the Waveshare autopling web UI / `/api/pling`, the
esp32-screen Home Assistant panel, Atech cloud events — runs in the emulator and talks to
real hosts (Home Assistant, price APIs, a browser on the Mac), with no root privileges.

**Status: done, over emulated 802.11.** Firmware associates with a virtual access point through
the unmodified Espressif blob (docs/wifi-plan.md), and the Ethernet traffic that comes out of the
MAC is handled by two layers in front of the host network:

- `esp-soc/src/net.rs` — the emulated subnet 10.0.2.0/24 (station 10.0.2.15, gateway 10.0.2.2,
  resolver 10.0.2.3): ARP, DHCP, ICMP echo, DNS and an SNTP server serving the host clock.
- `esp-soc/src/nat.rs` — `--net nat` (the default): everything addressed past the gateway is
  terminated in the emulator and relayed over ordinary host sockets, which is how Contiki-NG's
  NAT64 does it. A guest SYN becomes a `TcpStream::connect` on a worker thread, guest payload is
  written to that socket, socket reads come back as segments the emulator sequences, acknowledges
  and retransmits; UDP flows are a bound `UdpSocket` per (port, destination) with a reply path
  and an idle reaper. Name lookups are forwarded to the host's own first resolver from
  `/etc/resolv.conf`. `--net none` refuses outbound traffic instead (immediate RST).

No libslirp, no tun device, no entitlement, no root. TLS works: the panel fetches
`https://www.elprisetjustnu.se` and polls Home Assistant on the LAN. That needed the RSA/MPI
accelerator, SHA over GDMA (including SHA-384) and AES-CTR — see docs/peripherals.md.

Host-initiated TCP is the same relay in the other direction: `Nat::listen` / `adopt_listener`
accept on a listener and inject a SYN toward the guest (PATCHES.md #6). The esp32sim CLI has
no `--hostfwd` flag; NOTE's `note-emu --forward` is the caller.

## Not there yet

- **Multicast/mDNS**: not carried, so `esp-web.local` and Home Assistant discovery do not resolve;
  use IP addresses.
- **Real LAN presence**: a `--net tap`/vmnet backend would give the guest an address on the real
  network (and mDNS with it), at the cost of root or the macOS vmnet entitlement.
- **SoftAP station peer** (PATCHES #7–#9): an open-system station joins a BSS the guest is
  advertising and takes a DHCP lease. With a passphrase it also completes WPA2-PSK (CCMP,
  HMAC-SHA1) and then DHCP. `SoftApRelay` answers ARP for that address and carries browser
  TCP to the AP on `peer_tx` / `peer_eth`. This is not the 10.0.2.0 station NAT. There is
  no SAE and no vmnet backend.

## Alternatives considered

- **libslirp** (QEMU's user-mode NAT) instead of the Rust NAT: a C dependency and an FFI thread for
  behaviour we needed only a subset of. The subset turned out to be a few hundred lines.
- **An `esp_wifi` shim or the OpenCores Ethernet MAC** (`CONFIG_ETH_USE_OPENETH`, which IDF ships a
  driver for) instead of emulating the 802.11 MAC: cheaper, but it needs a firmware config change,
  so binaries would no longer be the ones that run on the board. Emulating the MAC kept "unmodified
  firmware" true; neither route is built.
