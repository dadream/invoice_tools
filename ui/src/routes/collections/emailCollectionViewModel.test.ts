import { describe, expect, it } from 'vitest'
import type { CollectedEmailMessage } from '../../lib/types'
import {
  collectionProcessResult,
  messageGroup,
  messagesForReviewGroup,
  receivedDate,
  sortCollectedMessages,
} from './emailCollectionViewModel'

function message(
  id: number,
  receivedAt: string | null,
  status: CollectedEmailMessage['status'],
  resolution: CollectedEmailMessage['resolution_status'] = 'open',
): CollectedEmailMessage {
  return {
    id,
    task_id: 1,
    mailbox_folder: 'INBOX',
    uid: id,
    message_id_sha256: null,
    sender: 'billing@example.test',
    subject: `mail-${id}`,
    received_at: receivedAt,
    status,
    resolution_status: resolution,
    error_category: null,
    created_at: '2026-08-01',
    updated_at: '2026-08-01',
    resolved_at: null,
    attachments: [],
  }
}

describe('email collection list view model', () => {
  it('displays only YYYY-MM-DD without timezone conversion', () => {
    expect(receivedDate('2026-06-30 23:59:59 +0800')).toBe('2026-06-30')
    expect(receivedDate('2026-06-01T00:01:00Z')).toBe('2026-06-01')
    expect(receivedDate(null)).toBe('日期未知')
  })

  it('sorts newest first and keeps missing dates last', () => {
    const sorted = sortCollectedMessages([
      message(1, '2026-06-01 08:00', 'not_relevant'),
      message(2, null, 'not_relevant'),
      message(3, '2026-06-30 20:00', 'not_relevant'),
    ], 'desc')
    expect(sorted.map((item) => item.id)).toEqual([3, 1, 2])
  })

  it('assigns every mail to exactly one of the three groups', () => {
    expect(messageGroup(message(1, null, 'manual_download'))).toBe('needs_action')
    expect(messageGroup(message(2, null, 'has_candidates'))).toBe('pending')
    expect(messageGroup(message(3, null, 'failed', 'ignored'))).toBe('reviewed')
    expect(messageGroup(message(4, null, 'materials_only'))).toBe('needs_action')
  })

  it('keeps classification in process result instead of the group label', () => {
    expect(collectionProcessResult(message(1, null, 'needs_confirmation'))).toContain('疑似开票通知')
    expect(collectionProcessResult(message(2, null, 'not_relevant', 'ignored'))).toBe('用户确认无关')
  })

  it('keeps next and previous navigation inside the entry group', () => {
    const source = [
      message(1, '2026-06-01', 'has_candidates'),
      message(2, '2026-06-02', 'manual_download'),
      message(3, '2026-06-03', 'has_candidates'),
      message(4, '2026-06-04', 'not_relevant', 'ignored'),
    ]
    expect(messagesForReviewGroup(source, 'pending', 'desc').map((item) => item.id)).toEqual([3, 1])
    expect(messagesForReviewGroup(source, 'needs_action', 'desc').map((item) => item.id)).toEqual([2])
    expect(messagesForReviewGroup(source, 'pending', 'desc', 2).map((item) => item.id)).toEqual([3, 2, 1])
  })
})
