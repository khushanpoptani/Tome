import { describe, expect, it } from 'vitest';
import appSource from './App.tsx?raw';

describe('Jobs screen clear-log controls', () => {
  it('requires the explicit scope confirmation before clearing', () => {
    expect(appSource).toContain("title: 'Clear job logs?'");
    expect(appSource).toContain("okLabel: 'Clear job logs'");
    expect(appSource).toContain('CLEAR_JOB_LOGS_CONFIRMATION');
    expect(appSource).toContain('if (!confirmed) return;');
  });

  it('refreshes after success and renders actionable failures', () => {
    expect(appSource).toMatch(
      /const result = await clearJobLogs\(profile\);\s+await refresh\(\);\s+setNotice\(clearJobLogsSummary\(result\)\);/,
    );
    expect(appSource).toContain('clearJobLogsErrorMessage(reason)');
    expect(appSource).toContain("'Clear job logs'");
  });
});
