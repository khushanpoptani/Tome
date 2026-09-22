import { describe, expect, it } from 'vitest';
import { reconcileOutput, sanitizedJobExport } from './inference';

describe('inference replay reconciliation', () => {
  it('deduplicates deltas and lets checkpoints repair gaps', () => {
    const first = reconcileOutput('', 0, {
      event_id: 1,
      job_id: 'job',
      event_type: 'inference.text_delta',
      payload: { output_sequence: 1, text: 'Hello' },
      occurred_at: '',
    });
    expect(
      reconcileOutput(first.text, first.sequence, {
        event_id: 2,
        job_id: 'job',
        event_type: 'inference.text_delta',
        payload: { output_sequence: 1, text: 'Hello' },
        occurred_at: '',
      }),
    ).toEqual(first);
    expect(
      reconcileOutput(first.text, first.sequence, {
        event_id: 3,
        job_id: 'job',
        event_type: 'inference.output_checkpoint',
        payload: { output_sequence: 3, text: 'Hello world!' },
        occurred_at: '',
      }),
    ).toEqual({ text: 'Hello world!', sequence: 3 });
  });

  it('removes credential-shaped fields from JSON exports', () => {
    const exported = sanitizedJobExport({
      output: 'safe',
      Authorization: 'Bearer secret',
      nested: { signed_url: 'https://example.invalid/?token=secret' },
    });
    expect(exported).toContain('safe');
    expect(exported).not.toContain('secret');
  });
});
