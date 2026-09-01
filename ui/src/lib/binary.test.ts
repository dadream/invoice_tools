import { describe, expect, it } from 'vitest'
import { isPngBytes, normalizeIpcBytes } from './binary'

describe('normalizeIpcBytes', () => {
  it('normalizes ArrayBuffer, typed arrays, arrays, and wrapped arrays', () => {
    expect([...normalizeIpcBytes(new Uint8Array([1, 2]).buffer)!]).toEqual([1, 2])
    expect([...normalizeIpcBytes(new Uint8Array([3, 4]))!]).toEqual([3, 4])
    expect([...normalizeIpcBytes([5, 6])!]).toEqual([5, 6])
    expect([...normalizeIpcBytes({ data: [7, 8] })!]).toEqual([7, 8])
  })

  it('rejects malformed byte payloads and validates the PNG signature', () => {
    expect(normalizeIpcBytes([0, 256])).toBeNull()
    expect(normalizeIpcBytes({ data: ['1'] })).toBeNull()
    expect(isPngBytes(new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10, 0]))).toBe(true)
    expect(isPngBytes(new Uint8Array([1, 2, 3]))).toBe(false)
  })
})
