import type { PendingInvoiceDocument } from './types'

type ConvertiblePendingDocument = Pick<
  PendingInvoiceDocument,
  'proposed_role' | 'detection_reason' | 'original_name'
>

/** Only a confidently identified Didi itinerary may create an expense without an e-invoice. */
export function canConvertDidiItinerary(document: ConvertiblePendingDocument): boolean {
  const reason = document.detection_reason.toLocaleLowerCase()
  return document.proposed_role === 'itinerary'
    && (reason === 'didi_itinerary_detected'
      || (reason === 'itinerary_detected'
        && document.original_name.toLocaleLowerCase().includes('滴滴')))
}

const PENDING_REASON_LABELS: Record<string, string> = {
  didi_itinerary_detected: '识别为滴滴行程单，可转为出租车费用',
  itinerary_detected: '识别为行程单，尚未匹配所属费用',
  detail_detected: '识别为消费明细，尚未匹配所属费用',
  hotel_folio_detected: '识别为酒店结账单，尚未匹配所属费用',
  supporting_detected: '识别为配套材料，尚未匹配所属费用',
  pdf_failed: 'PDF 内容读取失败，需要人工处理',
  ofd_failed: 'OFD 内容读取失败，需要人工处理',
  ocr_failed: '图片文字识别失败，需要人工处理',
  xml_failed: 'XML 内容读取失败，需要人工处理',
  parse_failed: '未识别为有效发票或配套材料',
}

/** Human-facing copy never exposes internal pipeline reason codes. */
export function pendingDocumentReasonLabel(reason: string): string {
  return PENDING_REASON_LABELS[reason.trim().toLocaleLowerCase()] ?? '需要人工判断材料用途'
}
