import { describe, expect, it } from 'vitest';
import { stageLabel } from './build-info';

describe('development scaffold', () => {
  it('identifies the active project stage', () => {
    expect(stageLabel).toBe('Development environment ready');
  });
});
