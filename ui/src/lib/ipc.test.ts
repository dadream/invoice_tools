import { describe, expect, it, vi, beforeEach } from 'vitest'

const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }))

const { invokeSafe, isAppError, toAppError, describeError } = await import('./ipc')

describe('isAppError', () => {
  it('accepts the backend contract', () => {
    expect(isAppError({ kind: 'validation', message: '姓名不能为空', recoverable: true })).toBe(true)
  })

  it('rejects plain strings and unknown kinds', () => {
    expect(isAppError('boom')).toBe(false)
    expect(isAppError({ kind: 'nope', message: 'x', recoverable: true })).toBe(false)
    expect(isAppError({ message: 'x' })).toBe(false)
  })
})

describe('toAppError', () => {
  it('passes through a valid AppError', () => {
    const err = { kind: 'network', message: '超时', recoverable: true } as const
    expect(toAppError(err)).toEqual(err)
  })

  it('downgrades unknown values to a non-recoverable internal error', () => {
    const err = toAppError('something exploded')
    expect(err.kind).toBe('internal')
    expect(err.recoverable).toBe(false)
    expect(err.message).toContain('something exploded')
  })

  it('handles Error instances', () => {
    expect(toAppError(new Error('kaboom')).message).toContain('kaboom')
  })
})

describe('invokeSafe', () => {
  it('returns ok on success', async () => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue('你好')
    const result = await invokeSafe<string>('greet', { name: '张三' })
    expect(result).toEqual({ ok: true, data: '你好' })
    expect(invokeMock).toHaveBeenCalledWith('greet', { name: '张三' })
  })

  it('returns a structured error when the command rejects', async () => {
    invokeMock.mockReset()
    invokeMock.mockRejectedValue({ kind: 'validation', message: '姓名不能为空', recoverable: true })
    try {
      const result = await invokeSafe('greet', { name: '' })
      expect(result.ok).toBe(false)
      if (!result.ok) {
        expect(result.error.kind).toBe('validation')
        expect(result.error.recoverable).toBe(true)
      }
    } catch (e) {
      throw new Error(`invokeSafe threw when it should not have: ${e}`)
    }
  })

  it('never throws, even on non-contract rejections', async () => {
    invokeMock.mockReset()
    invokeMock.mockRejectedValue('raw string failure')
    try {
      const result = await invokeSafe('greet')
      expect(result.ok).toBe(false)
      if (!result.ok) expect(result.error.kind).toBe('internal')
    } catch (e) {
      throw new Error(`invokeSafe threw when it should not have: ${e}`)
    }
  })
})

describe('describeError', () => {
  it('suggests retry for recoverable errors', () => {
    const text = describeError({ kind: 'network', message: '超时', recoverable: true })
    expect(text).toContain('超时')
    expect(text).toContain('重试')
  })

  it('points at logs for unrecoverable errors', () => {
    const text = describeError({ kind: 'internal', message: '崩了', recoverable: false })
    expect(text).toContain('日志')
  })
})
