import { describe, expect, it } from 'vitest';

import { currentLocalMonth, linkOnlyEmailNotice, monthDateRange } from './pipeline';

describe('linkOnlyEmailNotice', () => {
  it('gives a safe manual-download and local-import fallback', () => {
    const message = linkOnlyEmailNotice(3);

    expect(message).toContain('3 封');
    expect(message).toContain('不会自动打开或下载');
    expect(message).toContain('核对发件人和链接域名');
    expect(message).toContain('“本地文件”');
    expect(message).not.toMatch(/https?:\/\//i);
  });

  it('does not render an invalid count', () => {
    expect(linkOnlyEmailNotice(Number.NaN)).toContain('0 封');
    expect(linkOnlyEmailNotice(-1)).toContain('0 封');
  });
});

describe('pipeline month helpers', () => {
  it('uses the local calendar month', () => {
    expect(currentLocalMonth(new Date(2026, 5, 18, 23, 30))).toBe('2026-06');
  });

  it('returns the inclusive first and last day', () => {
    expect(monthDateRange('2026-06')).toEqual({
      start: '2026-06-01',
      end: '2026-06-30'
    });
    expect(monthDateRange('2028-02')).toEqual({
      start: '2028-02-01',
      end: '2028-02-29'
    });
    expect(monthDateRange('2027-02')).toEqual({
      start: '2027-02-01',
      end: '2027-02-28'
    });
  });

  it('rejects malformed and out-of-range months', () => {
    expect(monthDateRange('2026-6')).toBeNull();
    expect(monthDateRange('2026-00')).toBeNull();
    expect(monthDateRange('2026-13')).toBeNull();
  });
});
