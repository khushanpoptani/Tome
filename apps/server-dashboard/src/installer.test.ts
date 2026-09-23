import { describe, expect, it } from 'vitest';
import configurationSource from '../src-tauri/tauri.conf.json?raw';
import hooks from '../src-tauri/windows/installer-hooks.nsh?raw';

describe('Windows installer lifecycle', () => {
  it('leaves application launch to the standard NSIS finish page', () => {
    const postInstall = hooks.match(
      /!macro NSIS_HOOK_POSTINSTALL([\s\S]*?)!macroend/,
    )?.[1];

    expect(postInstall).toBeDefined();
    expect(postInstall).toContain('netsh advfirewall firewall add rule');
    expect(postInstall).not.toContain('ExecShell');
    expect(postInstall).not.toContain('ExecWait');
  });

  it('uses Tauri default NSIS pages and retains installer integration', () => {
    const configuration = JSON.parse(configurationSource) as {
      bundle: {
        targets: string[];
        windows: {
          nsis: Record<string, unknown>;
        };
      };
    };
    const nsis = configuration.bundle.windows.nsis;

    expect(configuration.bundle.targets).toContain('nsis');
    expect(nsis.installerHooks).toBe('windows/installer-hooks.nsh');
    expect(nsis.installMode).toBe('perMachine');
    expect(nsis.startMenuFolder).toBe('Tome');
    expect(nsis).not.toHaveProperty('template');
  });
});
