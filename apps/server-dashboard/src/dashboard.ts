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

export type OllamaState =
  | { state: 'not_installed' }
  | { state: 'installed_not_running'; installed_version: string | null }
  | { state: 'ready'; version: string; capability: 'version_api' }
  | { state: 'incompatible'; message: string }
  | { state: 'unreachable'; message: string }
  | { state: 'install_failed'; message: string }
  | { state: 'start_failed'; message: string };

export interface OllamaPresentation {
  title: string;
  detail: string;
  tone: 'ready' | 'attention' | 'error';
  action: 'install' | 'start' | 'retry' | null;
}

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

export function ollamaPresentation(ollama: OllamaState): OllamaPresentation {
  switch (ollama.state) {
    case 'not_installed':
      return {
        title: 'Ollama is required',
        detail:
          'Install the official Windows app to provide Tome’s local model runtime.',
        tone: 'attention',
        action: 'install',
      };
    case 'installed_not_running':
      return {
        title: 'Ollama is installed but stopped',
        detail: ollama.installed_version
          ? `Detected Ollama ${ollama.installed_version}. Start its loopback service to continue.`
          : 'Start its loopback service to continue.',
        tone: 'attention',
        action: 'start',
      };
    case 'ready':
      return {
        title: `Ollama ${ollama.version} is ready`,
        detail: 'Version API available on this PC only.',
        tone: 'ready',
        action: null,
      };
    case 'incompatible':
      return {
        title: 'Ollama API is incompatible',
        detail: ollama.message,
        tone: 'error',
        action: 'install',
      };
    case 'unreachable':
      return {
        title: 'Ollama is unreachable',
        detail: ollama.message,
        tone: 'error',
        action: 'retry',
      };
    case 'install_failed':
      return {
        title: 'Could not open the installer page',
        detail: ollama.message,
        tone: 'error',
        action: 'install',
      };
    case 'start_failed':
      return {
        title: 'Ollama did not start',
        detail: ollama.message,
        tone: 'error',
        action: 'start',
      };
  }
}
