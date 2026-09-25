import { describe, expect, it } from 'vitest';
import dialogPluginSource from '@tauri-apps/plugin-dialog?raw';
import capability from '../src-tauri/capabilities/default.json';

describe('packaged client capabilities', () => {
  it('allows the dialog command used by the installed confirm wrapper', () => {
    expect(dialogPluginSource).toContain("invoke('plugin:dialog|message'");
    expect(dialogPluginSource).toMatch(
      /async function confirm[\s\S]*?await messageCommand\(/,
    );
    expect(capability.windows).toEqual(['main']);
    expect(capability.permissions).toContain('dialog:allow-message');
    expect(capability.permissions).not.toContain('dialog:allow-confirm');
    expect(capability.permissions).not.toContain('dialog:default');
  });
});
