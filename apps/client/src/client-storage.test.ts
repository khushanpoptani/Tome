import { describe, expect, it } from 'vitest';
import { chatDownloadName, newProfile } from './client-storage';

describe('client storage contracts', () => {
  it('creates stable profile-shaped drafts without browser persistence', () => {
    const profile = newProfile({
      id: '0199a0ce-491b-7cc4-bbb7-9279f9067241',
      name: 'Studio',
    });
    expect(profile).toMatchObject({
      id: '0199a0ce-491b-7cc4-bbb7-9279f9067241',
      name: 'Studio',
      mode: 'lan',
      lastEventId: 0,
    });
  });

  it('generates portable export names without trusting chat titles as paths', () => {
    expect(
      chatDownloadName({
        id: '0199a0ce-491b-7cc4-bbb7-9279f9067241',
        title: '../../Quarterly notes',
      }),
    ).toBe('quarterly-notes-0199a0ce.tome.json');
  });
});
