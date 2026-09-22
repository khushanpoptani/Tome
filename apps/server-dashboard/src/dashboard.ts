export type AddressKind =
  'loopback' | 'lan' | 'tailscale' | 'public' | 'unusable';

export interface AddressView {
  interface_name: string;
  address: string;
  kind: AddressKind;
  url: string;
  active: boolean;
}

export type TailscaleState =
  | { state: 'connected'; hostname: string | null }
  | { state: 'not_installed' }
  | { state: 'not_signed_in' }
  | { state: 'no_address' };

export function groupAddresses(addresses: AddressView[]) {
  return {
    loopback: addresses.filter((address) => address.kind === 'loopback'),
    lan: addresses.filter((address) => address.kind === 'lan'),
    tailscale: addresses.filter((address) => address.kind === 'tailscale'),
  };
}

export function tailscaleMessage(tailscale: TailscaleState): string {
  switch (tailscale.state) {
    case 'connected':
      return tailscale.hostname
        ? `Connected as ${tailscale.hostname}`
        : 'Connected and ready';
    case 'not_installed':
      return 'Tailscale is not installed on this PC.';
    case 'not_signed_in':
      return 'Tailscale is installed but not signed in.';
    case 'no_address':
      return 'Tailscale is running, but no address is available yet.';
  }
}
