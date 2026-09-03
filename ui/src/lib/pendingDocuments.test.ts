import { describe, expect, it } from 'vitest'
import { canConvertDidiItinerary, pendingDocumentReasonLabel } from './pendingDocuments'

describe('pending document actions', () => {
  it('allows a detected Didi itinerary to create an expense', () => {
    expect(canConvertDidiItinerary({
      proposed_role: 'itinerary',
      detection_reason: 'itinerary_detected',
      original_name: '滴滴出行行程报销单.pdf',
    })).toBe(true)
  })

  it('prefers the persisted Didi detection even when the filename is generic', () => {
    expect(canConvertDidiItinerary({
      proposed_role: 'itinerary',
      detection_reason: 'didi_itinerary_detected',
      original_name: '2026-06-18-行程报销单.pdf',
    })).toBe(true)
  })

  it('does not offer conversion for generic supporting material', () => {
    expect(canConvertDidiItinerary({
      proposed_role: 'supporting',
      detection_reason: 'supporting_detected',
      original_name: '滴滴说明.pdf',
    })).toBe(false)
  })

  it('does not guess that an unknown itinerary belongs to Didi', () => {
    expect(canConvertDidiItinerary({
      proposed_role: 'itinerary',
      detection_reason: 'itinerary_detected',
      original_name: '行程单.pdf',
    })).toBe(false)
  })

  it('maps internal reason codes to Chinese product copy', () => {
    expect(pendingDocumentReasonLabel('didi_itinerary_detected')).toBe('识别为滴滴行程单，可转为出租车费用')
    expect(pendingDocumentReasonLabel('pdf_failed')).toBe('PDF 内容读取失败，需要人工处理')
  })

  it('does not expose unknown internal reason codes', () => {
    expect(pendingDocumentReasonLabel('future_internal_code')).toBe('需要人工判断材料用途')
  })
})
