import { describe, expect, it } from 'vitest';
import { groupAddresses, tailscaleMessage } from './dashboard';

describe('dashboard helpers', () => {
  it('shows LAN and Tailscale listeners at the same time', () => {
    const groups = groupAddresses([
      {
        interface_name: 'Ethernet',
        address: '192.168.1.20',
        kind: 'lan',
        url: 'http://192.168.1.20:7331',
        active: true,
      },
      {
        interface_name: 'Tailscale',
        address: '100.100.20.30',
        kind: 'tailscale',
        url: 'http://100.100.20.30:7331',
        active: true,
      },
    ]);

    expect(groups.lan).toHaveLength(1);
    expect(groups.tailscale).toHaveLength(1);
  });

  it('explains Tailscale setup states', () => {
    expect(tailscaleMessage({ state: 'not_installed' })).toContain(
      'not installed',
    );
    expect(tailscaleMessage({ state: 'not_signed_in' })).toContain(
      'not signed in',
    );
  });
});
